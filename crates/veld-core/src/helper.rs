use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// Overall timeout for a single request/response round-trip with the helper.
/// The helper bounds its own Caddy admin calls at ~10s, so this leaves margin
/// for a slow-but-working helper while still guaranteeing a caller (e.g. the
/// daemon's GC task) can never block forever on a wedged helper after sleep.
///
/// [`HelperCommand::Restart`] is the tightest consumer and the one to check
/// before shrinking this: the helper answers it only after a service-manager
/// query (5s) and an exec check (`BINARY_EXEC_CHECK_TIMEOUT`, 6s), so its worst
/// case is ~11s plus connect/write/read. Cutting this below that turns a slow
/// refusal into an apparent dead helper.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

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

/// The wire name of [`HelperCommand::Restart`].
///
/// A constant rather than two string literals because the two halves live in
/// different crates with nothing between them: `veld-core` emits it, and
/// `veld-helper`'s dispatch matches on it (a `&'static str` const is a legal
/// match pattern, so the compiler ties them rather than a test asserting they
/// agree). A typo on either side would otherwise degrade in silence — the helper
/// would answer `unknown command`, the CLI would ignore it as "an old helper",
/// and the update would fall back to a 45s wait and a sudo prompt with every test
/// still green. The older commands predate this and are still paired literals;
/// new ones should follow this shape.
pub const RESTART: &str = "restart";

/// Disable battery sleep, or renew an existing hold, for `lease_secs`.
///
/// The lease is the safety property, not a detail: `pmset -b disablesleep` is a
/// *durable* setting rather than an assertion, so a caller that dies without
/// renewing must lose it. See `veld-helper`'s `sleep` module.
///
/// Helpers older than the release that added this answer
/// `unknown command: hold_sleep_disabled`; callers must treat that as "this
/// machine has no battery coverage" and carry on with the unprivileged hold,
/// never as a failure of the keep-awake itself.
pub const HOLD_SLEEP_DISABLED: &str = "hold_sleep_disabled";

/// Re-enable battery sleep now rather than waiting out the lease. Same
/// version-skew rule as [`HOLD_SLEEP_DISABLED`].
pub const RELEASE_SLEEP_DISABLED: &str = "release_sleep_disabled";

/// Wire format: `{"command": "<name>", "args": {…}}`.
///
/// We implement [`Serialize`] manually so that the enum serialises into the
/// `command` + `args` object that veld-helper's server expects.
#[derive(Debug, Clone)]
pub enum HelperCommand {
    AddHost {
        hostname: String,
        ip: String,
    },
    RemoveHost {
        hostname: String,
    },
    AddRoute {
        route: serde_json::Value,
    },
    RemoveRoute {
        route_id: String,
    },
    /// Remove every Caddy route whose id starts with `prefix` (used to purge
    /// orphaned `veld-join-*` routes on daemon startup).
    RemoveRoutesByPrefix {
        prefix: String,
    },
    ReloadDns,
    CaddyStart,
    CaddyStop,
    Status,
    Shutdown,
    /// Exit so the service manager relaunches the helper onto a freshly
    /// installed binary, leaving Caddy running. Unlike [`Self::Shutdown`] this
    /// keeps every live URL served across the swap — it is how an unprivileged
    /// `veld update` restarts the *root* helper without sudo.
    ///
    /// Helpers older than 16.14 answer `unknown command: restart`; callers must
    /// treat that as "fall back to the binary watcher", not as a failure.
    Restart,
    /// Hold battery lid-closed sleep off for `lease_secs`, renewable by
    /// re-issuing. See [`HOLD_SLEEP_DISABLED`] for why there is a lease at all.
    HoldSleepDisabled {
        lease_secs: u64,
    },
    /// Drop the hold above immediately. See [`RELEASE_SLEEP_DISABLED`].
    ReleaseSleepDisabled,
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
            HelperCommand::RemoveRoutesByPrefix { prefix } => (
                "remove_routes_by_prefix",
                serde_json::json!({ "prefix": prefix }),
            ),
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
            HelperCommand::Restart => (RESTART, serde_json::Value::Object(Default::default())),
            HelperCommand::HoldSleepDisabled { lease_secs } => (
                HOLD_SLEEP_DISABLED,
                serde_json::json!({ "lease_secs": lease_secs }),
            ),
            HelperCommand::ReleaseSleepDisabled => (
                RELEASE_SLEEP_DISABLED,
                serde_json::Value::Object(Default::default()),
            ),
        };

        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("command", command)?;
        map.serialize_entry("args", &args)?;
        map.end()
    }
}

#[cfg(test)]
mod command_wire_tests {
    use super::{HOLD_SLEEP_DISABLED, HelperCommand, RELEASE_SLEEP_DISABLED};

    /// The command *name* is the whole contract, and getting it wrong fails
    /// silently in the worst possible direction: the helper answers
    /// `unknown command`, which callers are required to treat as "this machine
    /// has no battery coverage" and carry on — so a typo here ships as a feature
    /// that is quietly off for everyone, with nothing red anywhere.
    #[test]
    fn the_sleep_commands_serialise_to_the_names_the_helper_dispatches_on() {
        let hold = serde_json::to_value(HelperCommand::HoldSleepDisabled { lease_secs: 90 })
            .expect("serialisation cannot fail");
        assert_eq!(hold["command"], serde_json::json!(HOLD_SLEEP_DISABLED));
        // The lease has to arrive as a *number*: the helper reads it with
        // `as_u64`, so a stringified value would be refused as missing.
        assert_eq!(hold["args"]["lease_secs"], serde_json::json!(90));

        let release = serde_json::to_value(HelperCommand::ReleaseSleepDisabled)
            .expect("serialisation cannot fail");
        assert_eq!(
            release["command"],
            serde_json::json!(RELEASE_SLEEP_DISABLED)
        );
        assert_eq!(release["args"], serde_json::json!({}));
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
#[derive(Clone)]
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

    /// Send a command and receive the response, bounded by [`SEND_TIMEOUT`] so
    /// a dead/wedged helper cannot hang the caller indefinitely.
    async fn send(&self, command: &HelperCommand) -> Result<HelperResponse, HelperError> {
        match tokio::time::timeout(SEND_TIMEOUT, self.send_inner(command)).await {
            Ok(result) => result,
            Err(_) => Err(HelperError::ReadFailed(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "veld-helper did not respond within timeout",
            ))),
        }
    }

    /// Inner request/response round-trip without the timeout wrapper.
    async fn send_inner(&self, command: &HelperCommand) -> Result<HelperResponse, HelperError> {
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

    pub async fn remove_routes_by_prefix(
        &self,
        prefix: &str,
    ) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::RemoveRoutesByPrefix {
            prefix: prefix.to_owned(),
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

    /// Ask the helper to exit so its service manager relaunches it on the binary
    /// now on disk. See [`HelperCommand::Restart`] for why this is not
    /// `shutdown`, and why a `CommandError` here is a fallback signal rather
    /// than a failure.
    pub async fn restart(&self) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::Restart).await
    }

    /// Hold battery lid-closed sleep off for `lease_secs`, or renew the hold.
    ///
    /// Only meaningful against a **privileged** helper — the unprivileged one
    /// runs as the user and `pmset` will refuse it. Callers reach this through
    /// [`Self::connect_privileged`] rather than [`Self::connect`] for that
    /// reason.
    pub async fn hold_sleep_disabled(
        &self,
        lease_secs: u64,
    ) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::HoldSleepDisabled { lease_secs })
            .await
    }

    /// Drop the hold above. Idempotent, and best-effort at every call site: the
    /// lease expiring is the guarantee, and this is only the fast path.
    pub async fn release_sleep_disabled(&self) -> Result<HelperResponse, HelperError> {
        self.send(&HelperCommand::ReleaseSleepDisabled).await
    }

    /// Connect **only** to a privileged helper, never falling back to the user
    /// socket.
    ///
    /// [`Self::connect`] deliberately degrades to an unprivileged helper, which
    /// is right for routes and hosts — those work either way. It is wrong for
    /// anything that needs root: a user-socket helper would accept the request
    /// and fail inside `pmset`, turning "this machine cannot do that" into an
    /// error that reads like a bug. Asking on the system socket answers the
    /// capability question directly, since only a root-owned helper binds it.
    pub async fn connect_privileged() -> Result<Self, HelperError> {
        Self::try_connect(&system_socket_path()).await
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

    /// Connect to the helper at a specific socket path, verifying it is
    /// responsive with a bounded status check. Public wrapper over
    /// [`Self::try_connect`] for callers that know which socket a managed
    /// helper should be on (e.g. mode-aware connection in `ensure_helper`).
    pub async fn connect_to(socket_path: &Path) -> Result<Self, HelperError> {
        Self::try_connect(socket_path).await
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

    /// Both ports the helper's Caddy is listening on, as it reports them.
    ///
    /// **The authoritative answer, and the reason it is worth one round trip.**
    /// Everything else infers the pair from `~/.veld/setup.json`, and the two can
    /// disagree: a helper started by hand, a `veld setup privileged` that died
    /// after writing the mode, or a stray user helper on the high pair while the
    /// privileged LaunchDaemon is down (`veld doctor` reports exactly that state).
    /// The daemon's `Origin` allowlist has to know which pair is really in front —
    /// an origin it does not recognise is a WebSocket upgrade it refuses, and a
    /// browser cannot show the user why.
    ///
    /// `None` for a helper too old to report `http_port`, so a caller can keep its
    /// own fallback rather than being handed a plausible guess.
    pub async fn web_ports(&self) -> Result<Option<(u16, u16)>, HelperError> {
        let resp = self.status().await?;
        let data = resp.data;
        let get = |key: &str| {
            data.as_ref()
                .and_then(|d| d.get(key))
                .and_then(|v| v.as_u64())
        };
        Ok(match (get("https_port"), get("http_port")) {
            (Some(https), Some(http)) => Some((https as u16, http as u16)),
            _ => None,
        })
    }

    /// Query the running helper's version string from its status response.
    /// Older helpers that predate this field return `None`.
    pub async fn version(&self) -> Result<Option<String>, HelperError> {
        let resp = self.status().await?;
        Ok(resp
            .data
            .as_ref()
            .and_then(|d| d.get("version"))
            .and_then(|v| v.as_str())
            .map(str::to_owned))
    }
}
