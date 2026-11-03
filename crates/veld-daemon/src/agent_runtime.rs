//! Concrete inherited-account Codex adapter. No credential files or model turns.
//!
//! Remote ingress calls `describe_worktree` and `stage_launch`. A PTY executes
//! `launch` only after its host-selected login/interactive shell has started.
//! Every launch reads the immutable receipt again and consumes a one-shot claim.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use veld_core::agent_runtime::{
    self as contract, AccountInfo, AuthStatus, BillingKind, LaunchSelection, ModelInfo,
    OptionDescriptor, RuntimeCatalogue, RuntimeInfo, SessionSelection,
};
use veld_core::db::agent_runtime::{AgentLaunchRecord, runtime_digest};
use veld_core::db::{Db, WorktreeRecord};
use veld_core::remote::{ClientInfo, Descriptor};
use veld_core::runner::AdminRequest;

const RUNTIME_ID: &str = "codex";
const CONNECTION_ID: &str = "codex-inherited-chatgpt";
const PROBE_TIMEOUT: Duration = Duration::from_secs(25);
const ADMIN_TIMEOUT: Duration = Duration::from_secs(5);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PROBE_BYTES: usize = 4 * contract::MAX_JSON_BYTES;
const MAX_MODEL_PAGES: usize = 8;
const MODEL_PAGE_SIZE: usize = 64;
const REASONING: &str = "reasoningEffort";
const SERVICE_TIER: &str = "serviceTier";
// Codex's explicit opt-out from catalogue tiers, distinct from an absent default.
// https://github.com/openai/codex/blob/main/codex-rs/protocol/src/config_types.rs
const STANDARD_SERVICE_TIER: &str = "default";
static PROBE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

/// Catalogue in the registered checkout's login/interactive shell context.
/// The caller must authenticate before calling; this function accepts no path from a client.
pub async fn describe_worktree(worktree_id: i64) -> Result<RuntimeCatalogue> {
    let scope = checkout(worktree_id).await?;
    describe(&scope.directory).await
}

/// Host-only discovery primitive. Never expose `directory` through a remote API.
/// Shell startup retains inherited `CODEX_HOME` and `CLAUDE_CONFIG_DIR` unchanged.
pub async fn describe(directory: &Path) -> Result<RuntimeCatalogue> {
    let directory = directory.to_path_buf();
    let shell =
        crate::offload::blocking(|| -> Result<String> { Ok(Db::open()?.terminal_shell()) }).await?;
    Ok(probe(&directory, Some(&shell)).await?.catalogue)
}

/// Validate and persist a complete immutable selection before minting a PTY ticket.
/// The authenticated principal supplies `client_id`; the request supplies only selection and IDs.
pub async fn stage_launch(
    worktree_id: i64,
    session_id: &str,
    selection: &LaunchSelection,
    client_id: &str,
) -> Result<SessionSelection> {
    validate_session_id(session_id)?;
    let host = host_descriptor(Some(client_id)).await?;
    let scope = checkout(worktree_id).await?;
    let catalogue = describe(&scope.directory).await?;
    let resolved = contract::validate_selection(selection, &catalogue, None)?;
    let requested = AgentLaunchRecord {
        client_id: client_id.to_owned(),
        worktree_id,
        directory: directory_text(&scope.directory)?,
        session: SessionSelection {
            selection: resolved,
            provider_session_id: None,
            instance_id: host.instance_id,
            checkout_id: scope.identity,
        },
    };
    let id = session_id.to_owned();
    let expected = requested.clone();
    let stored = crate::offload::blocking(move || -> Result<AgentLaunchRecord> {
        let db = Db::open()?;
        db.reserve_agent_launch(&id, &expected)?
            .context("legacy terminal has no approved agent selection; use a new session ID")
    })
    .await?;
    ensure!(
        stored == requested,
        "session selection or owner differs from the approved record"
    );
    Ok(stored.session)
}

/// Read metadata for a same-holder attach, without inventing a launch or changing defaults.
/// The parent still verifies holder liveness and its terminal ownership receipt.
pub async fn session_selection(
    worktree_id: i64,
    session_id: &str,
    client_id: &str,
) -> Result<SessionSelection> {
    validate_session_id(session_id)?;
    let record = load_record(session_id).await?;
    ensure!(
        record.client_id == client_id,
        "agent session belongs to another client"
    );
    ensure!(
        record.worktree_id == worktree_id,
        "agent session belongs to another worktree"
    );
    let host = host_descriptor(Some(client_id)).await?;
    let scope = checkout(worktree_id).await?;
    validate_binding(&record, &scope, &host)?;
    Ok(record.session)
}

/// Execute once in the PTY's final shell context. On Unix, success replaces this process.
/// Dispatch this mode before normal daemon startup and print errors only to stderr.
pub async fn launch(session_id: &str) -> Result<()> {
    validate_session_id(session_id)?;
    let record = load_record(session_id).await?;
    ensure!(
        record.session.provider_session_id.is_none(),
        "native Codex resume is unsupported"
    );
    let host = host_descriptor(Some(&record.client_id)).await?;
    let scope = checkout(record.worktree_id).await?;
    validate_binding(&record, &scope, &host)?;
    let cwd = std::env::current_dir().context("cannot resolve final shell directory")?;
    ensure!(
        cwd.canonicalize()? == scope.directory,
        "final shell is in a different checkout"
    );

    // No second shell: this probe sees exactly the account/environment the TUI will inherit.
    let verified = probe(&scope.directory, None).await?;
    let request = contract::build_launch_request(
        &record.directory,
        &record.session,
        &verified.catalogue,
        None,
    )?;
    contract::validate_launch_request(&request, &verified.catalogue, None)?;
    let argv = codex_argv(&record.session.selection, &verified)?;

    // Recheck host scope after discovery, which can await a slow provider response.
    let host = host_descriptor(Some(&record.client_id)).await?;
    let scope = checkout(record.worktree_id).await?;
    validate_binding(&record, &scope, &host)?;
    let id = session_id.to_owned();
    let claimed = crate::offload::blocking(move || -> Result<bool> {
        Ok(Db::open()?.claim_agent_launch(&id, &record)?)
    })
    .await?;
    ensure!(
        claimed,
        "agent launch was already consumed; native resume is unavailable"
    );

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new("codex").args(argv).exec();
        Err(error).context("cannot execute the verified Codex launch")
    }
    #[cfg(not(unix))]
    {
        let _ = argv;
        bail!("verified Codex launch requires Unix");
    }
}

fn validate_session_id(id: &str) -> Result<()> {
    ensure!(
        veld_core::agent::valid_session_id(id),
        "invalid agent session ID"
    );
    Ok(())
}

async fn load_record(session_id: &str) -> Result<AgentLaunchRecord> {
    let id = session_id.to_owned();
    crate::offload::blocking(move || -> Result<AgentLaunchRecord> {
        Db::open()?
            .agent_launch(&id)?
            .context("session has no host-approved agent selection")
    })
    .await
}

struct Checkout {
    directory: PathBuf,
    identity: String,
}

async fn checkout(worktree_id: i64) -> Result<Checkout> {
    crate::offload::blocking(move || -> Result<Checkout> {
        let worktree = Db::open()?
            .get_worktree(worktree_id)?
            .context("registered worktree is unavailable")?;
        checkout_record(&worktree)
    })
    .await
}

fn checkout_record(worktree: &WorktreeRecord) -> Result<Checkout> {
    use std::os::unix::fs::MetadataExt;
    ensure!(
        worktree.trashed_at.is_empty(),
        "registered worktree is in trash"
    );
    let directory = Path::new(&worktree.path)
        .canonicalize()
        .context("registered worktree directory is unavailable")?;
    let metadata = std::fs::metadata(&directory)?;
    ensure!(metadata.is_dir(), "registered checkout is not a directory");
    let identity = digest_json(
        "veld-checkout-v1",
        &json!({
            "registryId": worktree.id, "registryPath": worktree.path,
            "repoRoot": worktree.repo_root, "registeredAt": worktree.created_at,
            "directory": directory_text(&directory)?, "device": metadata.dev(), "inode": metadata.ino(),
        }),
    )?;
    Ok(Checkout {
        directory,
        identity,
    })
}

fn validate_binding(
    record: &AgentLaunchRecord,
    checkout: &Checkout,
    host: &Descriptor,
) -> Result<()> {
    ensure!(
        record.session.instance_id == host.instance_id,
        "agent session belongs to another runner"
    );
    ensure!(
        record.session.checkout_id == checkout.identity,
        "registered checkout identity changed"
    );
    ensure!(
        record.directory == directory_text(&checkout.directory)?,
        "registered checkout path changed"
    );
    Ok(())
}

fn directory_text(directory: &Path) -> Result<String> {
    let text = directory
        .to_str()
        .context("checkout path is not UTF-8")?
        .to_owned();
    contract::validate_describe_request(&contract::DescribeRequest {
        directory: text.clone(),
    })?;
    Ok(text)
}

/// Read the live RunnerStore descriptor through its owner-only admin socket.
/// This avoids opening, creating, or modifying runner credential files in a helper.
async fn host_descriptor(client_id: Option<&str>) -> Result<Descriptor> {
    let database = crate::offload::blocking(Db::default_path).await?;
    let socket = veld_core::runner::admin_socket(&database)?;
    let response = admin(&socket, AdminRequest::Status).await?;
    let descriptor: Descriptor = serde_json::from_value(
        response
            .get("descriptor")
            .cloned()
            .context("runner descriptor is unavailable")?,
    )
    .map_err(|_| anyhow::anyhow!("runner descriptor is invalid"))?;
    if let Some(id) = client_id {
        let clients: Vec<ClientInfo> =
            serde_json::from_value(admin(&socket, AdminRequest::Clients).await?)
                .map_err(|_| anyhow::anyhow!("runner client metadata is invalid"))?;
        ensure!(
            clients.iter().any(|client| client.id == id),
            "remote client is no longer authorized"
        );
    }
    Ok(descriptor)
}

async fn admin(socket: &Path, request: AdminRequest) -> Result<Value> {
    tokio::time::timeout(ADMIN_TIMEOUT, async {
        let mut stream = UnixStream::connect(socket)
            .await
            .context("runner admin socket is unavailable")?;
        ensure!(
            stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
            "runner owner does not match"
        );
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        let mut reader = BufReader::new(stream);
        let raw = read_frame(&mut reader, contract::MAX_JSON_BYTES).await?;
        let response: Value = contract::decode_payload(&raw)?;
        ensure!(
            response.get("ok") == Some(&Value::Bool(true)),
            "runner admin request failed"
        );
        response
            .get("result")
            .cloned()
            .context("runner admin response is incomplete")
    })
    .await
    .context("runner admin request timed out")?
}

struct VerifiedCodex {
    catalogue: RuntimeCatalogue,
    workspace_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountRead {
    account: Option<Value>,
    requires_openai_auth: bool,
    workspace_routing: Option<WorkspaceRouting>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceRouting {
    chatgpt_account_id: String,
    backend_origin: String,
    account_routing_override: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelPage {
    data: Vec<CodexModel>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexModel {
    model: String,
    display_name: String,
    hidden: bool,
    default_reasoning_effort: String,
    supported_reasoning_efforts: Vec<ReasoningOption>,
    #[serde(default)]
    service_tiers: Vec<ServiceTier>,
    default_service_tier: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReasoningOption {
    reasoning_effort: String,
}

#[derive(Deserialize)]
struct ServiceTier {
    id: String,
}

async fn probe(directory: &Path, shell: Option<&str>) -> Result<VerifiedCodex> {
    directory_text(directory)?;
    tokio::time::timeout(PROBE_TIMEOUT, async {
        let _slot = PROBE_SLOTS
            .acquire()
            .await
            .context("Codex discovery is unavailable")?;
        let mut rpc = Rpc::spawn(directory, shell).await?;
        let result = discover(&mut rpc).await;
        let cleanup = rpc.stop().await;
        match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Err(error), Err(cleanup)) => {
                tracing::warn!(error = %cleanup, "Codex discovery cleanup also failed");
                Err(error)
            }
            (_, Err(error)) => Err(error),
        }
    })
    .await
    .context("Codex discovery timed out; inherited account is unavailable")?
}

async fn discover(rpc: &mut Rpc) -> Result<VerifiedCodex> {
    let initialized = rpc
        .request(
            "initialize",
            json!({
                "clientInfo": {"name": "veld_runtime", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": true},
            }),
        )
        .await?;
    rpc.send(json!({"method": "initialized"})).await?;
    let account = rpc
        .request("account/read", json!({"refreshToken": false}))
        .await?;
    let config = rpc
        .request("config/read", json!({"includeLayers": false}))
        .await?;
    let (account_id, workspace_id) = verified_identity(&account, &config)?;
    let mut models = Vec::new();
    let mut cursors = BTreeSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_MODEL_PAGES {
        let value = rpc
            .request(
                "model/list",
                json!({
                    "limit": MODEL_PAGE_SIZE, "includeHidden": false, "cursor": cursor,
                }),
            )
            .await?;
        let page: ModelPage = decode_value(value)?;
        for model in page.data {
            if !model.hidden {
                models.push(model_info(model, &config)?);
            }
        }
        ensure!(
            models.len() <= contract::MAX_MODELS,
            "Codex model catalogue is too large"
        );
        cursor = page.next_cursor;
        let Some(next) = &cursor else { break };
        ensure!(
            next.len() <= contract::MAX_JSON_BYTES && cursors.insert(next.clone()),
            "invalid Codex pagination"
        );
    }
    ensure!(
        cursor.is_none(),
        "Codex model pagination exceeded its limit"
    );
    ensure!(
        !models.is_empty(),
        "inherited ChatGPT account has no advertised models"
    );
    models.sort_by(|left, right| left.id.cmp(&right.id));
    let account_after = rpc
        .request("account/read", json!({"refreshToken": false}))
        .await?;
    ensure!(
        account == account_after,
        "Codex account changed during discovery"
    );
    let config_after = rpc
        .request("config/read", json!({"includeLayers": false}))
        .await?;
    ensure!(
        config == config_after,
        "Codex configuration changed during discovery"
    );
    let mut catalogue = RuntimeCatalogue {
        revision: "pending".into(),
        runtimes: vec![RuntimeInfo {
            id: RUNTIME_ID.into(),
            label: "Codex".into(),
            accounts: vec![AccountInfo {
                id: account_id,
                label: "Inherited ChatGPT account".into(),
                provider_connection_id: CONNECTION_ID.into(),
                auth_status: AuthStatus::Authenticated,
                identity_summary: Some("Verified inherited ChatGPT workspace".into()),
                billing_kind: BillingKind::Subscription,
                models,
            }],
        }],
    };
    catalogue.revision = digest_json(
        "veld-codex-catalogue-v1",
        &json!({
            "catalogue": catalogue, "account": account, "config": config,
            "runtime": initialized,
        }),
    )?;
    contract::validate_catalogue(&catalogue)?;
    Ok(VerifiedCodex {
        catalogue,
        workspace_id,
    })
}

fn verified_identity(account: &Value, config: &Value) -> Result<(String, String)> {
    let read: AccountRead = decode_value(account.clone())?;
    let details = read
        .account
        .context("Codex has no authenticated inherited account")?;
    ensure!(
        read.requires_openai_auth && details.get("type").and_then(Value::as_str) == Some("chatgpt"),
        "only an inherited ChatGPT account is supported; API-key identity is unavailable"
    );
    let email = details
        .get("email")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= 320 && !text.chars().any(char::is_control))
        .context("Codex account identity is incomplete")?;
    let routing = read
        .workspace_routing
        .context("Codex workspace identity is unavailable")?;
    ensure!(
        !routing.chatgpt_account_id.is_empty()
            && routing.chatgpt_account_id.len() <= 256
            && !routing.chatgpt_account_id.chars().any(char::is_control),
        "Codex workspace identity is invalid"
    );
    ensure!(
        routing.backend_origin == "https://chatgpt.com",
        "Codex backend identity is unsupported"
    );
    ensure!(
        matches!(
            config.pointer("/config/model_provider"),
            None | Some(Value::Null)
        ) || config
            .pointer("/config/model_provider")
            .and_then(Value::as_str)
            == Some("openai"),
        "inherited Codex provider is unsupported"
    );
    // A custom OpenAI provider can route elsewhere while account/read still shows ChatGPT.
    ensure!(
        config.pointer("/config/model_providers/openai").is_none(),
        "custom OpenAI provider is unsupported"
    );
    ensure!(
        matches!(
            config.pointer("/config/openai_base_url"),
            None | Some(Value::Null)
        ),
        "custom OpenAI endpoint is unsupported"
    );
    ensure!(
        matches!(
            config.pointer("/config/chatgpt_base_url"),
            None | Some(Value::Null)
        ) || config
            .pointer("/config/chatgpt_base_url")
            .and_then(Value::as_str)
            .is_some_and(|url| url.trim_end_matches('/') == "https://chatgpt.com/backend-api"),
        "custom ChatGPT endpoint is unsupported"
    );
    let id = digest_json(
        "veld-codex-account-v1",
        &json!({
            "email": email, "workspace": routing.chatgpt_account_id, "backend": routing.backend_origin,
            "routing": routing.account_routing_override,
        }),
    )?;
    Ok((id, routing.chatgpt_account_id))
}

fn model_info(model: CodexModel, config: &Value) -> Result<ModelInfo> {
    let mut efforts: Vec<Value> = model
        .supported_reasoning_efforts
        .into_iter()
        .map(|option| Value::String(option.reasoning_effort))
        .collect();
    efforts.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    let mut options = vec![OptionDescriptor {
        id: REASONING.into(),
        values: efforts,
        default: Some(Value::String(model.default_reasoning_effort)),
    }];
    let configured_tier = config
        .pointer("/config/service_tier")
        .and_then(Value::as_str);
    let mut tiers: Vec<Value> = model
        .service_tiers
        .into_iter()
        .map(|tier| Value::String(tier.id))
        .collect();
    tiers.push(Value::String(STANDARD_SERVICE_TIER.into()));
    tiers.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    tiers.dedup();
    options.push(OptionDescriptor {
        id: SERVICE_TIER.into(),
        values: tiers,
        default: configured_tier
            .map(str::to_owned)
            .or(model.default_service_tier)
            .map(Value::String),
    });
    Ok(ModelInfo {
        id: model.model,
        label: model.display_name,
        options,
    })
}

fn codex_argv(selection: &LaunchSelection, verified: &VerifiedCodex) -> Result<Vec<String>> {
    let resolved = contract::validate_selection(selection, &verified.catalogue, None)?;
    ensure!(
        &resolved == selection,
        "Codex options are not fully resolved"
    );
    let mut args = vec![
        "--no-daemon".into(),
        "--model".into(),
        selection.model_id.clone(),
    ];
    for (key, value) in [
        ("model_provider", "openai"),
        ("forced_login_method", "chatgpt"),
        (
            "forced_chatgpt_workspace_id",
            verified.workspace_id.as_str(),
        ),
    ] {
        args.extend([
            "--config".into(),
            format!("{key}={}", serde_json::to_string(value)?),
        ]);
    }
    for (id, value) in &selection.options {
        let key = match id.as_str() {
            REASONING => "model_reasoning_effort",
            SERVICE_TIER => "service_tier",
            _ => bail!("Codex option has no launch mapping"),
        };
        let text = value.as_str().context("Codex option is not a string")?;
        args.extend([
            "--config".into(),
            format!("{key}={}", serde_json::to_string(text)?),
        ]);
    }
    Ok(args)
}

fn digest_json(domain: &str, value: &Value) -> Result<String> {
    Ok(runtime_digest(domain, &serde_json::to_vec(value)?))
}

fn decode_value<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| anyhow::anyhow!("Codex runtime metadata is invalid"))
}

struct Rpc {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    request_id: u64,
    bytes_read: usize,
}

impl Rpc {
    async fn spawn(directory: &Path, shell: Option<&str>) -> Result<Self> {
        let marker = format!("veld-codex-ready-{}", uuid::Uuid::new_v4().simple());
        let mut command = match shell {
            Some(shell) => {
                ensure!(
                    matches!(
                        Path::new(shell).file_name().and_then(|name| name.to_str()),
                        Some("bash" | "zsh" | "sh" | "dash")
                    ),
                    "host shell is unsupported for verified Codex discovery"
                );
                let mut command = Command::new(shell);
                command.args(["-l", "-i"]).env("TERM", "xterm-256color");
                command
            }
            None => {
                // Account identity cannot prove API-key or alternate endpoint routing.
                ensure!(
                    std::env::var_os("OPENAI_BASE_URL").is_none(),
                    "custom OpenAI endpoint is unsupported"
                );
                let mut command = Command::new("codex");
                command.args(["app-server", "--listen", "stdio://"]);
                command
            }
        };
        command
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .spawn()
            .context("cannot start Codex discovery in the host shell")?;
        let input = child
            .stdin
            .take()
            .context("Codex discovery has no input channel")?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .context("Codex discovery has no output channel")?,
        );
        let mut rpc = Self {
            child,
            input,
            output,
            request_id: 0,
            bytes_read: 0,
        };
        if shell.is_some() {
            // Stdin triggers prompt/directory hooks. No account directories or environment are rewritten.
            rpc.input
                .write_all(
                    format!(
                        "if [ \"${{OPENAI_BASE_URL+x}}\" = x ]; then exit 78; fi; \
                 printf '%s\\n' '{marker}'; exec /usr/bin/env codex app-server --listen stdio://\n"
                    )
                    .as_bytes(),
                )
                .await?;
            rpc.input.flush().await?;
            loop {
                let frame = rpc.frame().await?;
                if frame
                    .strip_suffix(b"\n")
                    .unwrap_or(&frame)
                    .ends_with(marker.as_bytes())
                {
                    break;
                }
            }
        }
        Ok(rpc)
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        ensure!(
            bytes.len() < contract::MAX_JSON_BYTES,
            "Codex request is too large"
        );
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .await
            .context("cannot write Codex runtime request")?;
        self.input.flush().await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_id += 1;
        let id = self.request_id;
        self.send(json!({"id": id, "method": method, "params": params}))
            .await?;
        loop {
            let frame = self.frame().await?;
            let response: Value = contract::decode_payload(&frame)?;
            if response.get("id") == Some(&json!(id)) {
                ensure!(
                    response.get("error").is_none(),
                    "Codex runtime request failed: {method}"
                );
                return response
                    .get("result")
                    .cloned()
                    .context("Codex response has no result");
            }
            ensure!(
                response.get("id").is_none()
                    && response.get("method").and_then(Value::as_str).is_some(),
                "unexpected Codex RPC response or server request"
            );
        }
    }

    async fn frame(&mut self) -> Result<Vec<u8>> {
        let frame = read_frame(&mut self.output, contract::MAX_JSON_BYTES).await?;
        self.bytes_read += frame.len();
        ensure!(
            self.bytes_read <= MAX_PROBE_BYTES,
            "Codex discovery output exceeded its limit"
        );
        Ok(frame)
    }

    async fn stop(&mut self) -> Result<()> {
        self.kill_group()?;
        tokio::time::timeout(CLEANUP_TIMEOUT, self.child.wait())
            .await
            .context("Codex discovery cleanup timed out")??;
        Ok(())
    }

    fn kill_group(&self) -> Result<()> {
        if let Some(id) = self.child.id() {
            let result = unsafe { libc::kill(-(id as i32), libc::SIGKILL) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                // A provider can exit before cleanup. ESRCH means its group is already gone.
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error).context("cannot terminate Codex discovery group");
                }
            }
        }
        Ok(())
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        // The probe never starts inference. Kill its whole group, including stalled shell startup.
        if let Err(error) = self.kill_group() {
            tracing::warn!(error = %error, "cannot terminate dropped Codex discovery");
        }
    }
}

async fn read_frame<R: AsyncBufRead + Unpin>(reader: &mut R, limit: usize) -> Result<Vec<u8>> {
    let mut frame = Vec::new();
    loop {
        let bytes = reader
            .fill_buf()
            .await
            .context("cannot read runtime control channel")?;
        ensure!(
            !bytes.is_empty(),
            "runtime control channel closed before a complete response"
        );
        let count = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |index| index + 1);
        ensure!(
            frame.len() + count <= limit,
            "runtime control frame exceeded its limit"
        );
        frame.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if frame.last() == Some(&b'\n') {
            return Ok(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn account(workspace: &str) -> Value {
        json!({
            "account": {"type": "chatgpt", "email": "fixture@example.invalid", "planType": "pro"},
            "requiresOpenaiAuth": true,
            "workspaceRouting": {
                "chatgptAccountId": workspace, "backendOrigin": "https://chatgpt.com",
                "accountRoutingOverride": "NO_CONSTRAINT",
            },
        })
    }

    fn config() -> Value {
        json!({"config": {
            "model_provider": null, "model_providers": {},
            "openai_base_url": null, "chatgpt_base_url": null, "service_tier": null,
        }})
    }

    fn model() -> CodexModel {
        decode_value(json!({
            "id": "picker-alias", "model": "fixture-runtime-model", "displayName": "Fixture model",
            "hidden": false, "defaultReasoningEffort": "fixture-effort",
            "supportedReasoningEfforts": [{"reasoningEffort": "fixture-effort"}],
            "serviceTiers": [{"id": "fixture-tier"}], "defaultServiceTier": "fixture-tier",
        }))
        .unwrap()
    }

    fn verified() -> VerifiedCodex {
        let (account_id, workspace_id) =
            verified_identity(&account("workspace-a"), &config()).unwrap();
        VerifiedCodex {
            catalogue: RuntimeCatalogue {
                revision: "fixture-revision".into(),
                runtimes: vec![RuntimeInfo {
                    id: RUNTIME_ID.into(),
                    label: "Codex".into(),
                    accounts: vec![AccountInfo {
                        id: account_id,
                        label: "Inherited ChatGPT account".into(),
                        provider_connection_id: CONNECTION_ID.into(),
                        auth_status: AuthStatus::Authenticated,
                        identity_summary: None,
                        billing_kind: BillingKind::Subscription,
                        models: vec![model_info(model(), &config()).unwrap()],
                    }],
                }],
            },
            workspace_id,
        }
    }

    fn selection(verified: &VerifiedCodex) -> LaunchSelection {
        LaunchSelection {
            runtime_id: RUNTIME_ID.into(),
            account_id: verified.catalogue.runtimes[0].accounts[0].id.clone(),
            provider_connection_id: CONNECTION_ID.into(),
            model_id: "fixture-runtime-model".into(),
            catalogue_revision: verified.catalogue.revision.clone(),
            options: BTreeMap::new(),
        }
    }

    #[test]
    fn identity_requires_workspace_and_user_and_never_uses_a_directory_as_an_account() {
        let original = verified_identity(&account("workspace-a"), &config()).unwrap();
        assert_eq!(
            original,
            verified_identity(&account("workspace-a"), &config()).unwrap()
        );
        assert_ne!(
            original.0,
            verified_identity(&account("workspace-b"), &config())
                .unwrap()
                .0
        );
        let mut changed = account("workspace-a");
        changed["account"]["email"] = json!("other@example.invalid");
        assert_ne!(
            original.0,
            verified_identity(&changed, &config()).unwrap().0
        );
        changed["account"]["email"] = Value::Null;
        assert!(verified_identity(&changed, &config()).is_err());
        changed = account("workspace-a");
        changed["workspaceRouting"] = Value::Null;
        assert!(verified_identity(&changed, &config()).is_err());
        changed["account"] = json!({"type": "apiKey"});
        assert!(verified_identity(&changed, &config()).is_err());
    }

    #[test]
    fn alternate_providers_and_endpoints_are_unavailable() {
        let original = account("workspace-a");
        let mut official = config();
        official["config"]["chatgpt_base_url"] = json!("https://chatgpt.com/backend-api/");
        assert!(verified_identity(&original, &official).is_ok());
        for (key, value) in [
            ("model_provider", json!("custom")),
            (
                "model_providers",
                json!({"openai": {"base_url": "https://fixture.invalid"}}),
            ),
            ("openai_base_url", json!("https://fixture.invalid")),
            ("chatgpt_base_url", json!("https://fixture.invalid")),
        ] {
            let mut changed = config();
            changed["config"][key] = value;
            assert!(verified_identity(&original, &changed).is_err());
        }
    }

    #[test]
    fn explicit_standard_tier_is_supported_without_catalogue_advertisement() {
        let mut configuration = config();
        configuration["config"]["service_tier"] = json!("default");
        let info = model_info(model(), &configuration).unwrap();
        let tier = info
            .options
            .iter()
            .find(|option| option.id == SERVICE_TIER)
            .unwrap();
        assert_eq!(tier.default, Some(json!("default")));
        assert!(tier.values.contains(&json!("default")));
        assert!(tier.values.contains(&json!("fixture-tier")));
        let mut catalogue = verified().catalogue;
        catalogue.runtimes[0].accounts[0].models = vec![info];
        contract::validate_catalogue(&catalogue).unwrap();
        configuration["config"]["service_tier"] = json!("unadvertised-tier");
        catalogue.runtimes[0].accounts[0].models =
            vec![model_info(model(), &configuration).unwrap()];
        assert!(contract::validate_catalogue(&catalogue).is_err());
    }

    #[test]
    fn runtime_values_map_to_exact_tui_arguments_after_defaults_are_persisted() {
        let verified = verified();
        let unresolved = selection(&verified);
        assert!(codex_argv(&unresolved, &verified).is_err());
        let resolved =
            contract::validate_selection(&unresolved, &verified.catalogue, None).unwrap();
        let args = codex_argv(&resolved, &verified).unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "fixture-runtime-model"])
        );
        for expected in [
            "model_reasoning_effort=\"fixture-effort\"",
            "service_tier=\"fixture-tier\"",
            "forced_login_method=\"chatgpt\"",
            "forced_chatgpt_workspace_id=\"workspace-a\"",
        ] {
            assert!(args.iter().any(|arg| arg == expected));
        }
        assert_eq!(args[0], "--no-daemon");
        assert!(!args.iter().any(|arg| arg == "resume" || arg == "--last"));
        let mut changed = resolved.clone();
        changed.catalogue_revision = "changed".into();
        assert!(codex_argv(&changed, &verified).is_err());
        changed = resolved;
        changed
            .options
            .insert(REASONING.into(), json!("unadvertised"));
        assert!(codex_argv(&changed, &verified).is_err());
    }

    #[test]
    fn catalogue_revision_covers_identity_models_defaults_and_configuration() {
        let verified = verified();
        let data = json!({"catalogue": verified.catalogue, "account": account("workspace-a"), "config": config()});
        let original = digest_json("revision", &data).unwrap();
        for pointer in [
            "/account/workspaceRouting/chatgptAccountId",
            "/catalogue/runtimes/0/accounts/0/models/0/id",
            "/catalogue/runtimes/0/accounts/0/models/0/options/0/default",
            "/config/config/service_tier",
        ] {
            let mut changed = data.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("changed");
            assert_ne!(original, digest_json("revision", &changed).unwrap());
        }
    }

    #[test]
    fn checkout_replacement_and_trash_invalidate_a_binding() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("checkout");
        std::fs::create_dir(&root).unwrap();
        let record: WorktreeRecord = serde_json::from_value(json!({
            "id": 1, "repo_root": "/fixture-repo", "path": root.to_str().unwrap(),
            "branch": "fixture", "alias": "fixture", "display_name": "", "emoji": "",
            "marker_color": "", "is_main": true, "created_at": "fixture-created",
            "lane": "", "sort_position": null, "trashed_at": "", "trash_error": "", "adopted": true,
        }))
        .unwrap();
        let original = checkout_record(&record).unwrap();
        std::fs::rename(&root, directory.path().join("old-checkout")).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert_ne!(
            original.identity,
            checkout_record(&record).unwrap().identity
        );
        let mut trashed = record;
        trashed.trashed_at = "fixture-trash".into();
        assert!(checkout_record(&trashed).is_err());
    }

    #[tokio::test]
    async fn control_frames_are_bounded_and_incomplete_frames_fail() {
        let mut reader = BufReader::new(&b"12345\n"[..]);
        assert!(read_frame(&mut reader, 5).await.is_err());
        let mut reader = BufReader::new(&b"123"[..]);
        assert!(read_frame(&mut reader, 5).await.is_err());
        let mut reader = BufReader::new(&b"ok\nnext\n"[..]);
        assert_eq!(read_frame(&mut reader, 5).await.unwrap(), b"ok\n");
        assert_eq!(read_frame(&mut reader, 5).await.unwrap(), b"next\n");
    }

    async fn mock_rpc(response: &str) -> Rpc {
        // A local protocol fixture only. It cannot launch Codex or submit a model turn.
        let script = format!(
            "read request; printf '%s\\n' '{}'",
            response.replace('\'', "'\\''")
        );
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", &script])
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Rpc {
            child,
            input,
            output,
            request_id: 0,
            bytes_read: 0,
        }
    }

    #[tokio::test]
    async fn rpc_skips_notifications_and_never_exposes_provider_errors() {
        let mut rpc =
            mock_rpc("{\"method\":\"notice\"}\n{\"id\":1,\"result\":{\"fixture\":true}}").await;
        assert_eq!(
            rpc.request("account/read", json!({})).await.unwrap(),
            json!({"fixture": true})
        );
        rpc.stop().await.unwrap();
        let mut rpc =
            mock_rpc("{\"id\":1,\"error\":{\"message\":\"fixture-private-provider-output\"}}")
                .await;
        let error = rpc.request("account/read", json!({})).await.unwrap_err();
        assert!(!format!("{error:#}").contains("fixture-private-provider-output"));
        rpc.stop().await.unwrap();
    }
}
