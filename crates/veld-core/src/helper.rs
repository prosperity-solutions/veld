use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// System socket path (used by privileged helper running as LaunchDaemon/systemd).
pub fn system_socket_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/var/run/veld-helper.sock")
    } else {
        PathBuf::from("/run/veld-helper.sock")
    }
}

/// User socket path (used by unprivileged helper running as user process).
pub fn user_socket_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".veld").join("helper.sock"))
        .unwrap_or_else(|| PathBuf::from("/tmp/veld-helper.sock"))
}

/// Default socket path for veld-helper.
#[deprecated(note = "use system_socket_path() or user_socket_path() instead")]
pub fn default_socket_path() -> PathBuf {
    system_socket_path()
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum HelperError {
    #[error("failed to connect to veld-helper at {path}: {source}")]
    ConnectionFailed {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to send command to veld-helper: {0}")]
    SendFailed(#[source] std::io::Error),

    #[error("failed to read response from veld-helper: {0}")]
    ReadFailed(#[source] std::io::Error),

    #[error("veld-helper returned an error: {0}")]
    CommandError(String),

    #[error("failed to parse veld-helper response: {0}")]
    ParseError(#[source] serde_json::Error),
}

// ---------------------------------------------------------------------------
// Protocol types
// ---------------------------------------------------------------------------

/// Wire format: `{"command": "<name>", "args": {…}}`.
///
/// We implement [`Serialize`] manually so that the enum serialises into the
/// `command` + `args` object that veld-helper's server expects.
#[derive(Debug, Clone)]
pub enum HelperCommand {
    AddHost { hostname: String, ip: String },
    RemoveHost { hostname: String },
    AddRoute { route: serde_json::Value },
    RemoveRoute { route_id: String },
    ReloadDns,
    CaddyStart,
    CaddyStop,
    Status,
    Shutdown,
}

impl Serialize for HelperCommand {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let (command, args): (&str, serde_json::Value) = match self {
            HelperCommand::AddHost { hostname, ip } => (
                "add_host",
                serde_json::json!({ "hostname": hostname, "ip": ip }),
            ),
            HelperCommand::RemoveHost { hostname } => {
                ("remove_host", serde_json::json!({ "hostname": hostname }))
            }
            HelperCommand::AddRoute { route } => ("add_route", route.clone()),
            HelperCommand::RemoveRoute { route_id } => {
                ("remove_route", serde_json::json!({ "route_id": route_id }))
            }
            HelperCommand::ReloadDns => {
                ("reload_dns", serde_json::Value::Object(Default::default()))
            }
            HelperCommand::CaddyStart => {
                ("caddy_start", serde_json::Value::Object(Default::default()))
            }
            HelperCommand::CaddyStop => {
                ("caddy_stop", serde_json::Value::Object(Default::default()))
            }
            HelperCommand::Status => ("status", serde_json::Value::Object(Default::default())),
            HelperCommand::Shutdown => ("shutdown", serde_json::Value::Object(Default::default())),
        };

        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("command", command)?;
        map.serialize_entry("args", &args)?;
        map.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Client for communicating with the veld-helper daemon over a Unix socket.
pub struct HelperClient {
    socket_path: PathBuf,
}

impl HelperClient {
    pub fn new(socket_path: &Path) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
        }
    }

    /// Create a client using the default socket path.
    #[allow(deprecated)]
    pub fn default_client() -> Self {
        Self::new(&default_socket_path())
    }

    /// Send a command and receive the response.
    async fn send(&self, command: &HelperCommand) -> Result<HelperResponse, HelperError> {
        let mut stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
            HelperError::ConnectionFailed {
                path: self.socket_path.clone(),
                source: e,
            }
        })?;

        // Write the JSON command followed by a newline delimiter.
        let payload = serde_json::to_vec(command).expect("command serialization cannot fail");
        stream
            .write_all(&payload)
            .await
            .map_err(HelperError::SendFailed)?;
        stream
            .write_all(b"\n")
            .await
            .map_err(HelperError::SendFailed)?;
        stream.shutdown().await.map_err(HelperError::SendFailed)?;

        // Read the response.
        let mut buf = Vec::new();
        stream
            .read_to_end(&mut buf)
            .await
            .map_err(HelperError::ReadFailed)?;

        let response: HelperResponse =
            serde_json::from_slice(&buf).map_err(HelperError::ParseError)?;

        if !response.ok {
            return Err(HelperError::CommandError(
                response.error.unwrap_or_else(|| "unknown error".to_owned()),
            ));
        }

        Ok(response)
    }

    // -- Convenience methods --------------------------------------------------

    pub async fn add_host(&self, hostname: &str, ip: &str) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::AddHost {
            hostname: hostname.to_owned(),
            ip: ip.to_owned(),
        })
        .await
    }

    pub async fn remove_host(&self, hostname: &str) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::RemoveHost {
            hostname: hostname.to_owned(),
        })
        .await
    }

    pub async fn add_route(&self, route: serde_json::Value) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::AddRoute { route }).await
    }

    pub async fn remove_route(&self, route_id: &str) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::RemoveRoute {
            route_id: route_id.to_owned(),
        })
        .await
    }

    pub async fn reload_dns(&self) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::ReloadDns).await
    }

    pub async fn caddy_start(&self) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::CaddyStart).await
    }

    pub async fn caddy_stop(&self) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::CaddyStop).await
    }

    pub async fn status(&self) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::Status).await
    }

    pub async fn shutdown(&self) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::Shutdown).await
    }

    /// Connect to the helper, trying system socket first, then user socket.
    /// Returns the connected client or an error if neither socket is reachable.
    pub async fn connect() -> Result<Self, HelperError> {
        // Try system socket first (privileged mode).
        let system = system_socket_path();
        if let Ok(client) = Self::try_connect(&system).await {
            return Ok(client);
        }
        // Try user socket (unprivileged mode).
        let user = user_socket_path();
        if let Ok(client) = Self::try_connect(&user).await {
            return Ok(client);
        }
        Err(HelperError::ConnectionFailed {
            path: user,
            source: std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "no helper reachable on system or user socket",
            ),
        })
    }

    /// Try to connect and verify the helper is responsive (status check).
    async fn try_connect(socket_path: &Path) -> Result<Self, HelperError> {
        let client = Self::new(socket_path);
        // Use a timeout to avoid hanging on a wedged helper.
        match tokio::time::timeout(std::time::Duration::from_secs(3), client.status()).await {
            Ok(Ok(_)) => Ok(client),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(HelperError::ConnectionFailed {
                path: socket_path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "helper status check timed out",
                ),
            }),
        }
    }

    /// Query the helper's HTTPS port from its status response.
    pub async fn https_port(&self) -> Result<u16, HelperError> {
        let resp = self.status().await?;
        Ok(resp
            .data
            .as_ref()
            .and_then(|d| d.get("https_port"))
            .and_then(|v| v.as_u64())
            .unwrap_or(443) as u16)
    }
}
