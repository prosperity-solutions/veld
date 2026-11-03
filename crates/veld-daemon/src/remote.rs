//! Authenticated remote ingress. The engine remains on its existing loopback API.
//!
//! Credentials are checked before a request can reach that API. The proxy owns
//! every remote stream, so revocation closes both HTTP and WebSocket transports
//! without cancelling the work owned by the engine.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};

#[path = "remote_preview.rs"]
mod preview;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tracing::warn;
use veld_core::remote::{PairRequest, RemoteError, RunnerStore};
use veld_core::runner::{AdminRequest, RunnerConfig};

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
const MAX_ADMIN_BYTES: usize = 4096;
const MAX_TICKETS: usize = 1024;
const TICKET_LIFETIME: Duration = Duration::from_secs(60);

struct Control {
    store: RunnerStore,
    clients: HashMap<String, watch::Sender<bool>>,
    tickets: HashMap<String, (String, String, Instant)>,
}

impl Control {
    fn close_clients(&mut self) {
        for (_, sender) in self.clients.drain() {
            sender.send_replace(true);
        }
        self.tickets.clear();
    }

    fn revoke(&mut self, client_id: &str) -> Result<bool, RemoteError> {
        let result = self.store.revoke(client_id);
        if matches!(result, Err(RemoteError::Durability(_))) {
            self.close_clients();
        }
        if matches!(result, Ok(true)) {
            if let Some(sender) = self.clients.remove(client_id) {
                sender.send_replace(true);
            }
            self.tickets.retain(|_, (owner, _, _)| owner != client_id);
        }
        result
    }
}

#[derive(Clone)]
struct Remote {
    control: Arc<Mutex<Control>>,
    origin: String,
    authority: String,
    upstream: String,
    http: reqwest::Client,
}

#[derive(Clone)]
struct Principal {
    id: String,
    revoked: watch::Receiver<bool>,
}

impl Principal {
    async fn revoked(mut self) {
        while !*self.revoked.borrow_and_update() {
            if self.revoked.changed().await.is_err() {
                return;
            }
        }
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({"error": message}))).into_response()
}

impl Remote {
    fn authorize(&self, headers: &HeaderMap) -> Result<Principal, Box<Response>> {
        if headers.get_all(header::HOST).iter().count() != 1
            || headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(&self.authority)
        {
            return Err(Box::new(error(
                StatusCode::FORBIDDEN,
                "unexpected runner host",
            )));
        }
        if headers.get_all(header::ORIGIN).iter().count() > 1
            || headers
                .get(header::ORIGIN)
                .is_some_and(|origin| origin.to_str().ok() != Some(&self.origin))
        {
            return Err(Box::new(error(
                StatusCode::FORBIDDEN,
                "unexpected request origin",
            )));
        }
        if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
            return Err(Box::new(error(
                StatusCode::UNAUTHORIZED,
                "one client credential is required",
            )));
        }
        let credential = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "pair this client first"))?;
        let mut control = self.control.lock().expect("runner control poisoned");
        let id = control
            .store
            .authenticate(credential)
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "client access is unavailable"))?;
        let revoked = control
            .clients
            .entry(id.clone())
            .or_insert_with(|| watch::channel(false).0)
            .subscribe();
        Ok(Principal { id, revoked })
    }
}

/// Start only when this instance has an explicit remote configuration.
pub async fn run(database: PathBuf, config: RunnerConfig) -> Result<()> {
    if config.daemon_port != veld_core::instance::daemon_port() {
        bail!("runner daemon port does not match its installed configuration");
    }
    let endpoint = reqwest::Url::parse(&config.endpoint).context("invalid remote endpoint")?;
    let host = endpoint
        .host_str()
        .context("remote endpoint needs a host")?;
    let authority = match endpoint.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let store_dir = veld_core::runner::state_directory(&database);
    let configured_endpoint = config.endpoint.clone();
    let store =
        crate::offload::blocking(move || RunnerStore::open(&store_dir, &configured_endpoint))
            .await?;
    let remote = Remote {
        control: Arc::new(Mutex::new(Control {
            store,
            clients: HashMap::new(),
            tickets: HashMap::new(),
        })),
        origin: endpoint.origin().ascii_serialization(),
        authority,
        upstream: veld_core::instance::daemon_base(),
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()?,
    };
    let admin_path = veld_core::runner::admin_socket(&database)?;
    let admin = bind_admin(&admin_path).await?;
    let listener =
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.remote_port)).await?;
    let app = router(remote.clone());
    tokio::select! {
        result = axum::serve(listener, app) => result.context("runner HTTP server failed"),
        result = serve_admin(admin, remote) => result,
    }
}

fn router(remote: Remote) -> Router {
    Router::new()
        .route("/remote/descriptor", get(descriptor))
        .route("/remote/pair", post(pair))
        .route("/remote/previews", get(preview::list))
        .route("/remote/preview", any(preview::proxy))
        .route("/remote/info", get(remote_info))
        .route("/remote/agents/{worktree_id}", get(runtime_catalogue))
        .route("/remote/agents/{worktree_id}/launch", post(stage_agent))
        .fallback(proxy)
        .with_state(remote)
}

async fn remote_info(State(remote): State<Remote>, headers: HeaderMap) -> Response {
    if let Err(response) = remote.authorize(&headers) {
        return *response;
    }
    axum::Json(
        remote
            .control
            .lock()
            .expect("runner control poisoned")
            .store
            .descriptor(),
    )
    .into_response()
}

async fn runtime_catalogue(
    State(remote): State<Remote>,
    headers: HeaderMap,
    axum::extract::Path(worktree_id): axum::extract::Path<i64>,
) -> Response {
    let principal = match remote.authorize(&headers) {
        Ok(p) => p,
        Err(response) => return *response,
    };
    tokio::select! {
        _ = principal.revoked() => error(StatusCode::UNAUTHORIZED, "client access revoked"),
        result = crate::agent_runtime::describe_worktree(worktree_id) => match result {
            Ok(catalogue) => axum::Json(catalogue).into_response(),
            Err(cause) => { warn!(error = %cause, "runner account discovery failed"); error(StatusCode::SERVICE_UNAVAILABLE, "Account discovery failed. Check Codex availability and login on the runner.") },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentRequest {
    session_id: String,
    selection: veld_core::agent_runtime::LaunchSelection,
}

async fn stage_agent(
    State(remote): State<Remote>,
    headers: HeaderMap,
    axum::extract::Path(worktree_id): axum::extract::Path<i64>,
    axum::Json(request): axum::Json<AgentRequest>,
) -> Response {
    let principal = match remote.authorize(&headers) {
        Ok(p) => p,
        Err(response) => return *response,
    };
    if !headers.contains_key("x-veld-request") {
        return error(StatusCode::FORBIDDEN, "missing X-Veld-Request header");
    }
    tokio::select! {
        _ = principal.clone().revoked() => error(StatusCode::UNAUTHORIZED, "client access revoked"),
        result = crate::agent_runtime::stage_launch(worktree_id, &request.session_id, &request.selection, &principal.id) => match result {
            Ok(selection) => axum::Json(selection).into_response(),
            Err(cause) => { warn!(error = %cause, "runner agent selection refused"); error(StatusCode::CONFLICT, "The host could not verify this selection. Refresh accounts and select again.") },
        }
    }
}

async fn bind_admin(path: &std::path::Path) -> Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
            bail!("runner admin socket path is not owned by this user");
        }
        if UnixStream::connect(path).await.is_ok() {
            bail!("runner admin socket is already active");
        }
        std::fs::remove_file(path).context("cannot remove stale runner socket")?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

async fn serve_admin(listener: UnixListener, remote: Remote) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let remote = remote.clone();
        tokio::spawn(async move {
            if let Err(error) = admin_request(stream, remote).await {
                warn!(%error, "runner admin request failed");
            }
        });
    }
}

async fn admin_request(stream: UnixStream, remote: Remote) -> Result<()> {
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        bail!("runner owner required");
    }
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut data = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let bytes = reader.fill_buf().await?;
            if bytes.is_empty() {
                bail!("incomplete runner request");
            }
            let count = bytes
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |n| n + 1);
            if data.len() + count > MAX_ADMIN_BYTES {
                bail!("runner request is too large");
            }
            data.extend_from_slice(&bytes[..count]);
            reader.consume(count);
            if data.last() == Some(&b'\n') {
                return Ok(());
            }
        }
    })
    .await??;
    let request: AdminRequest = serde_json::from_slice(&data)?;
    let result = if let AdminRequest::Revoke { client_id } = request {
        revoke_client(remote, client_id).await
    } else {
        crate::offload::blocking(move || -> Result<serde_json::Value> {
        let mut control = remote.control.lock().expect("runner control poisoned");
        match request {
            AdminRequest::Status => Ok(serde_json::json!({"descriptor":control.store.descriptor(),"upstream":remote.upstream})),
            AdminRequest::Pair => Ok(serde_json::json!({"invitation":control.store.invite(chrono::Utc::now().timestamp())?.encode()?})),
            AdminRequest::Clients => Ok(serde_json::to_value(control.store.clients())?),
            AdminRequest::Revoke { .. } => unreachable!("revocation is handled asynchronously"),
        }
    }).await
    };
    let response = match result {
        Ok(value) => serde_json::json!({"ok":true,"result":value}),
        Err(error) => serde_json::json!({"ok":false,"error":error.to_string()}),
    };
    write.write_all(&serde_json::to_vec(&response)?).await?;
    write.write_all(b"\n").await?;
    Ok(())
}

async fn revoke_client(remote: Remote, client_id: String) -> Result<serde_json::Value> {
    let (result, clients) = crate::offload::blocking(move || {
        let mut control = remote.control.lock().expect("runner control poisoned");
        let all_clients = control
            .store
            .clients()
            .into_iter()
            .map(|client| client.id)
            .collect();
        let result = control.revoke(&client_id);
        let clients = if matches!(result, Err(RemoteError::Durability(_))) {
            all_clients
        } else {
            vec![client_id]
        };
        (result, clients)
    })
    .await;
    fence_input(clients).await?;
    Ok(serde_json::json!({"revoked":result?}))
}

async fn fence_input(clients: Vec<String>) -> Result<()> {
    let mut failures = Vec::new();
    for client in clients {
        if let Err(error) = crate::http_server::pty::revoke_remote_client(&client).await {
            failures.push(error.to_string());
        }
    }
    if !failures.is_empty() {
        bail!("input revocation failed: {}", failures.join("; "));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Challenge {
    nonce: String,
}

async fn descriptor(State(remote): State<Remote>, Query(query): Query<Challenge>) -> Response {
    match remote
        .control
        .lock()
        .expect("runner control poisoned")
        .store
        .prove(&query.nonce)
    {
        Ok(proof) => axum::Json(proof).into_response(),
        Err(_) => error(StatusCode::BAD_REQUEST, "invalid identity challenge"),
    }
}

async fn pair(State(remote): State<Remote>, headers: HeaderMap, body: Body) -> Response {
    if headers
        .get(header::ORIGIN)
        .is_some_and(|o| o.to_str().ok() != Some(&remote.origin))
        || headers.get_all(header::ORIGIN).iter().count() > 1
        || headers.get_all(header::HOST).iter().count() != 1
        || headers.get("x-veld-request").is_none()
        || headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(&remote.authority)
    {
        return error(StatusCode::FORBIDDEN, "unexpected pairing request");
    }
    let bytes = match to_bytes(body, MAX_ADMIN_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "pairing request is too large",
            );
        }
    };
    let request = match serde_json::from_slice::<PairRequest>(&bytes) {
        Ok(request) => request,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid pairing request"),
    };
    let (result, clients) = crate::offload::blocking(move || {
        let mut control = remote.control.lock().expect("runner control poisoned");
        let clients = control
            .store
            .clients()
            .into_iter()
            .map(|client| client.id)
            .collect();
        let result = control
            .store
            .redeem(request, chrono::Utc::now().timestamp());
        if matches!(result, Err(RemoteError::Durability(_))) {
            control.close_clients();
            (result, clients)
        } else {
            (result, Vec::new())
        }
    })
    .await;
    if let Err(cause) = fence_input(clients).await {
        warn!(error = %cause, "pairing failure could not fence terminal input");
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "runner access is unavailable",
        );
    }
    match result {
        Ok(response) => axum::Json(response).into_response(),
        Err(_) => error(
            StatusCode::UNAUTHORIZED,
            "invitation is invalid, expired, or already used",
        ),
    }
}

fn allowed(path: &str) -> bool {
    path == "/ide"
        || path == "/api/health"
        || [
            "/api/engine/",
            "/api/repos",
            "/api/worktrees",
            "/api/pty/",
            "/api/ide/",
            "/api/environments",
            "/api/logs/",
            "/api/stats",
            "/api/settings",
            "/api/promotions",
            "/api/files/",
            "/api/file-pane/",
            "/api/changes",
            "/api/config/",
            "/api/shells",
            "/api/lanes",
            "/api/lane-order",
            "/api/repo-order",
            "/api/worktree-order",
            "/api/worktree-emoji",
            "/api/trash",
            "/api/outfit/",
            "/api/caffeinate",
            "/api/db-health",
            "/api/shares",
            "/api/claude-mod/check",
        ]
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

async fn proxy(State(remote): State<Remote>, request: Request) -> Response {
    let principal = match remote.authorize(request.headers()) {
        Ok(p) => p,
        Err(response) => return *response,
    };
    if !allowed(request.uri().path()) {
        return error(StatusCode::NOT_FOUND, "route is unavailable remotely");
    }
    if request.method() != Method::GET
        && request.method() != Method::HEAD
        && request.headers().get("x-veld-request").is_none()
    {
        return error(StatusCode::FORBIDDEN, "missing X-Veld-Request header");
    }
    if request.headers().get(header::UPGRADE).is_some() {
        let (mut parts, body) = request.into_parts();
        let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(upgrade) => upgrade,
            Err(_) => return error(StatusCode::BAD_REQUEST, "invalid socket upgrade"),
        };
        let request = Request::from_parts(parts, body);
        return websocket(remote, principal, upgrade, request).await;
    }
    http_proxy(remote, principal, request).await
}

fn hop_header(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "host"
            | "origin"
            | "connection"
            | "upgrade"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "keep-alive"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "x-veld-remote-client"
    )
}

async fn http_proxy(remote: Remote, principal: Principal, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, MAX_REQUEST_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "request is too large"),
    };
    let url = format!(
        "{}{}",
        remote.upstream,
        parts.uri.path_and_query().map_or("/", |p| p.as_str())
    );
    let mut builder = remote.http.request(parts.method, url).body(bytes);
    for (name, value) in &parts.headers {
        if !hop_header(name.as_str()) {
            builder = builder.header(name, value);
        }
    }
    builder = builder.header("x-veld-remote-client", &principal.id);
    let response = tokio::select! {
        _ = principal.clone().revoked() => return error(StatusCode::UNAUTHORIZED, "client access revoked"),
        result = builder.send() => match result {
            Ok(response) => response,
            Err(error) => { warn!(%error, "remote runner upstream request failed"); return error_response(); }
        }
    };
    let status = response.status();
    let mut headers = response.headers().clone();
    for name in [
        "connection",
        "transfer-encoding",
        "content-length",
        "set-cookie",
    ] {
        headers.remove(name);
    }
    if status.is_redirection() {
        return error(StatusCode::BAD_GATEWAY, "runner API redirect refused");
    }
    if status.is_success() && matches!(parts.uri.path(), "/api/pty/tickets" | "/api/ide/tickets") {
        return ticket_response(remote, principal, parts.uri.path(), response).await;
    }
    let mut output = Response::new(Body::from_stream(
        response.bytes_stream().take_until(principal.revoked()),
    ));
    *output.status_mut() = status;
    *output.headers_mut() = headers;
    output
}

fn error_response() -> Response {
    error(StatusCode::BAD_GATEWAY, "runner engine is unavailable")
}

async fn ticket_response(
    remote: Remote,
    principal: Principal,
    path: &str,
    response: reqwest::Response,
) -> Response {
    let value = match response.json::<serde_json::Value>().await {
        Ok(value) => value,
        Err(_) => return error_response(),
    };
    let Some(ticket) = value.get("ticket").and_then(|v| v.as_str()) else {
        return error_response();
    };
    let mut control = remote.control.lock().expect("runner control poisoned");
    control
        .tickets
        .retain(|_, (_, _, expires)| *expires > Instant::now());
    if *principal.revoked.borrow() {
        return error(StatusCode::UNAUTHORIZED, "client access revoked");
    }
    if control.tickets.len() >= MAX_TICKETS {
        return error(StatusCode::TOO_MANY_REQUESTS, "too many pending sockets");
    }
    control.tickets.insert(
        ticket.to_owned(),
        (
            principal.id,
            path.to_owned(),
            Instant::now() + TICKET_LIFETIME,
        ),
    );
    axum::Json(value).into_response()
}

async fn websocket(
    remote: Remote,
    principal: Principal,
    upgrade: WebSocketUpgrade,
    request: Request,
) -> Response {
    let path = request.uri().path();
    let purpose = match path {
        "/api/pty/attach" => "/api/pty/tickets",
        "/api/ide/channel" => "/api/ide/tickets",
        _ => return error(StatusCode::NOT_FOUND, "unknown remote socket"),
    };
    let parsed = reqwest::Url::parse(&format!("http://localhost{}", request.uri()));
    let ticket = parsed.ok().and_then(|url| {
        url.query_pairs()
            .find(|(key, _)| key == "ticket")
            .map(|(_, value)| value.into_owned())
    });
    let valid = {
        let mut control = remote.control.lock().expect("runner control poisoned");
        ticket
            .as_ref()
            .and_then(|ticket| control.tickets.remove(ticket))
            .is_some_and(|(owner, kind, expires)| {
                owner == principal.id && kind == purpose && expires > Instant::now()
            })
    };
    if !valid || *principal.revoked.borrow() {
        return error(StatusCode::FORBIDDEN, "invalid socket ticket");
    }
    let url = format!(
        "{}{}",
        remote.upstream.replacen("http://", "ws://", 1),
        request.uri()
    );
    let mut upstream = match url.into_client_request() {
        Ok(request) => request,
        Err(_) => return error_response(),
    };
    let Ok(origin) = remote.upstream.parse() else {
        return error_response();
    };
    upstream.headers_mut().insert(header::ORIGIN, origin);
    let Ok(client_id) = principal.id.parse() else {
        return error_response();
    };
    upstream
        .headers_mut()
        .insert("x-veld-remote-client", client_id);
    let socket = tokio::select! {
        _ = principal.clone().revoked() => return error(StatusCode::UNAUTHORIZED, "client access revoked"),
        result = tokio_tungstenite::connect_async(upstream) => match result { Ok((socket, _)) => socket, Err(_) => return error_response() }
    };
    upgrade.max_message_size(MAX_REQUEST_BYTES).on_upgrade(move |client| async move {
        tokio::select! {
            _ = principal.revoked() => {},
            result = bridge(client, socket) => if let Err(error) = result { warn!(%error, "remote socket closed with an error"); },
        }
    })
}

async fn bridge(
    client: WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<()> {
    use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let input = async {
        while let Some(frame) = client_rx.next().await {
            let frame = match frame? {
                Message::Text(v) => UpstreamMessage::Text(v.to_string().into()),
                Message::Binary(v) => UpstreamMessage::Binary(v),
                Message::Ping(v) => UpstreamMessage::Ping(v),
                Message::Pong(v) => UpstreamMessage::Pong(v),
                Message::Close(_) => break,
            };
            upstream_tx.send(frame).await?;
        }
        Ok::<(), anyhow::Error>(())
    };
    let output = async {
        while let Some(frame) = upstream_rx.next().await {
            let frame = match frame? {
                UpstreamMessage::Text(v) => Message::Text(v.to_string().into()),
                UpstreamMessage::Binary(v) => Message::Binary(v),
                UpstreamMessage::Ping(v) => Message::Ping(v),
                UpstreamMessage::Pong(v) => Message::Pong(v),
                UpstreamMessage::Close(_) => break,
                UpstreamMessage::Frame(_) => continue,
            };
            client_tx.send(frame).await?;
        }
        Ok::<(), anyhow::Error>(())
    };
    tokio::select! { result = input => result, result = output => result }
}

#[cfg(test)]
mod tests {
    use super::*;
    use veld_core::remote::{PairResponse, generate_nonce, verify_proof};

    const ORIGIN: &str = "https://runner.example.ts.net:19896";
    const AUTHORITY: &str = "runner.example.ts.net:19896";

    struct Harness {
        remote: Remote,
        url: String,
        tasks: Vec<tokio::task::JoinHandle<()>>,
        _state: tempfile::TempDir,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            for task in &self.tasks {
                task.abort();
            }
        }
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), task)
    }

    impl Harness {
        async fn new() -> Self {
            let upstream = Router::new()
                .route("/api/health", get(|headers: HeaderMap| async move {
                    axum::Json(serde_json::json!({
                        "client": headers.get("x-veld-remote-client").unwrap().to_str().unwrap(),
                        "authorization": headers.contains_key(header::AUTHORIZATION),
                    }))
                }))
                .route("/api/pty/tickets", post(|| async {
                    axum::Json(serde_json::json!({"ticket":uuid::Uuid::new_v4().to_string()}))
                }))
                .route("/api/pty/attach", get(|upgrade: WebSocketUpgrade| async {
                    upgrade.on_upgrade(|mut socket| async move {
                        while let Some(Ok(frame)) = socket.next().await {
                            if socket.send(frame).await.is_err() { break; }
                        }
                    })
                }))
                .route("/api/logs/stream", get(|| async {
                    let first = futures_util::stream::once(async { Ok::<_, std::io::Error>("ready\n") });
                    Body::from_stream(first.chain(futures_util::stream::pending()))
                }));
            let (upstream, upstream_task) = serve(upstream).await;
            let state = tempfile::tempdir().unwrap();
            let remote = Remote {
                control: Arc::new(Mutex::new(Control {
                    store: RunnerStore::open(&state.path().join("identity"), ORIGIN).unwrap(),
                    clients: HashMap::new(),
                    tickets: HashMap::new(),
                })),
                origin: ORIGIN.to_owned(),
                authority: AUTHORITY.to_owned(),
                upstream,
                http: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap(),
            };
            let (url, task) = serve(router(remote.clone())).await;
            Self {
                remote,
                url,
                tasks: vec![task, upstream_task],
                _state: state,
            }
        }

        fn client(&self) -> PairResponse {
            let mut control = self.remote.control.lock().unwrap();
            let now = chrono::Utc::now().timestamp();
            let invitation = control.store.invite(now).unwrap();
            control
                .store
                .redeem(
                    PairRequest {
                        secret: invitation.secret,
                        label: "test client".into(),
                    },
                    now,
                )
                .unwrap()
        }

        fn request(
            &self,
            method: Method,
            path: &str,
            client: &PairResponse,
        ) -> reqwest::RequestBuilder {
            self.remote
                .http
                .request(method, format!("{}{path}", self.url))
                .header(header::HOST, AUTHORITY)
                .bearer_auth(&client.credential)
                .header("x-veld-request", "1")
        }

        async fn ticket(&self, client: &PairResponse) -> String {
            self.request(Method::POST, "/api/pty/tickets", client)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()["ticket"]
                .as_str()
                .unwrap()
                .to_owned()
        }

        async fn socket(
            &self,
            client: &PairResponse,
            ticket: &str,
        ) -> Result<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            tokio_tungstenite::tungstenite::Error,
        > {
            let mut request = format!(
                "{}/api/pty/attach?ticket={ticket}",
                self.url.replace("http:", "ws:")
            )
            .into_client_request()
            .unwrap();
            request
                .headers_mut()
                .insert(header::HOST, AUTHORITY.parse().unwrap());
            request
                .headers_mut()
                .insert(header::ORIGIN, ORIGIN.parse().unwrap());
            request.headers_mut().insert(
                header::AUTHORIZATION,
                format!("Bearer {}", client.credential).parse().unwrap(),
            );
            tokio_tungstenite::connect_async(request)
                .await
                .map(|(socket, _)| socket)
        }
    }

    #[tokio::test]
    async fn pairing_requires_a_valid_invitation_and_cannot_replay() {
        let harness = Harness::new().await;
        let nonce = generate_nonce();
        let proof = harness
            .remote
            .http
            .get(format!("{}/remote/descriptor?nonce={nonce}", harness.url))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let descriptor = harness.remote.control.lock().unwrap().store.descriptor();
        verify_proof(&proof, &descriptor, &nonce).unwrap();
        let invitation = harness
            .remote
            .control
            .lock()
            .unwrap()
            .store
            .invite(chrono::Utc::now().timestamp())
            .unwrap();
        let body = serde_json::json!({"secret":invitation.secret,"label":"Mac"});
        for expected in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
            let response = harness
                .remote
                .http
                .post(format!("{}/remote/pair", harness.url))
                .header(header::HOST, AUTHORITY)
                .header("x-veld-request", "1")
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[tokio::test]
    async fn proxy_enforces_host_origin_authentication_and_principal_injection() {
        let harness = Harness::new().await;
        let client = harness.client();
        let response = harness
            .request(Method::GET, "/api/health", &client)
            .header("x-veld-remote-client", "spoofed")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(value["client"], client.client_id);
        assert_eq!(value["authorization"], false);
        assert_eq!(
            harness
                .request(Method::GET, "/api/health", &client)
                .header(header::HOST, "other.ts.net")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            harness
                .request(Method::GET, "/api/health", &client)
                .header(header::ORIGIN, "https://evil.example")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            harness
                .request(Method::GET, "/api/health", &client)
                .bearer_auth("wrong")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            harness
                .request(Method::POST, "/api/pick-directory", &client)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn ticket_cannot_cross_clients_or_survive_revocation() {
        let harness = Harness::new().await;
        let client = harness.client();
        let other = harness.client();
        let ticket = harness.ticket(&client).await;
        assert!(harness.socket(&other, &ticket).await.is_err());
        let ticket = harness.ticket(&client).await;
        harness
            .remote
            .control
            .lock()
            .unwrap()
            .revoke(&client.client_id)
            .unwrap();
        assert!(harness.socket(&client, &ticket).await.is_err());
        assert_eq!(
            harness
                .request(Method::GET, "/api/health", &other)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn revoke_closes_active_socket_and_http_stream() {
        use tokio_tungstenite::tungstenite::Message as Frame;
        let harness = Harness::new().await;
        let client = harness.client();
        let ticket = harness.ticket(&client).await;
        let mut socket = harness.socket(&client, &ticket).await.unwrap();
        socket.send(Frame::Text("hello".into())).await.unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Frame::Text("hello".into())
        );
        assert!(harness.socket(&client, &ticket).await.is_err());
        let mut stream = harness
            .request(Method::GET, "/api/logs/stream", &client)
            .send()
            .await
            .unwrap()
            .bytes_stream();
        assert_eq!(stream.next().await.unwrap().unwrap(), "ready\n");
        harness
            .remote
            .control
            .lock()
            .unwrap()
            .revoke(&client.client_id)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            assert!(stream.next().await.is_none());
            assert!(!matches!(
                socket.next().await,
                Some(Ok(Frame::Text(_) | Frame::Binary(_)))
            ));
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn admin_binding_never_deletes_a_regular_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("admin.sock");
        std::fs::write(&path, "keep this file").unwrap();
        assert!(bind_admin(&path).await.is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep this file");
    }
}
