//! Shared types for peer-to-peer environment sharing.
//!
//! These types are used by both the daemon (which runs the iroh endpoint and
//! forwards traffic) and the CLI (which encodes/decodes tickets and talks to the
//! daemon's control API). Keeping them in `veld-core` avoids duplicating the
//! wire format. The transport itself (iroh) lives only in the daemon.
//!
//! See `RFC-p2p-sharing.md` and `PLAN-p2p-implementation.md`.

use base64::prelude::*;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// How a host resolves incoming join requests to one of its shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Auto-approve the first token-valid joiner, pin it, hold the rest.
    #[default]
    First,
    /// Every join waits for explicit host approval (dashboard / CLI).
    Manual,
    /// Approve every token-valid joiner (multi-viewer; least restrictive).
    Auto,
}

/// One service exposed by a share. The `hostname` is the *literal* host from the
/// origin's `NodeState.url`, shipped verbatim so the consumer reproduces the
/// exact same URL (redirects, cookies, and CORS then work unmodified).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedNode {
    /// Node name (e.g. `frontend`).
    pub node: String,
    /// Variant name (e.g. `local`).
    pub variant: String,
    /// Literal hostname to reproduce on the consumer (no scheme, no port); used
    /// for the consumer's Caddy route match and DNS entry.
    pub hostname: String,
    /// The origin's full URL, shown to the consumer as the address to open.
    /// Valid verbatim on the consumer because both peers run the same setup mode
    /// (identical scheme + port).
    pub url: String,
    /// Host-local TCP port the origin's service listens on (the tunnel target
    /// the host dials).
    pub upstream_port: u16,
}

/// The set of services a host is sharing, plus lifetime metadata. Carried inside
/// the ticket so the consumer can reproduce every URL locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareManifest {
    /// The origin run this share was minted from.
    pub run_id: Uuid,
    /// Run name (for display and matching a share to its run card).
    pub run: String,
    /// Project name (for display / namespacing).
    pub project: String,
    /// Shared services.
    pub nodes: Vec<SharedNode>,
    /// Unix seconds when the share was created.
    pub created_at: i64,
    /// Unix seconds when the share expires.
    pub expires_at: i64,
}

/// A 32-byte bearer secret embedded in a ticket. A host serves a connection only
/// if it presents the matching capability (gate 1 of the security model).
///
/// Comparison is constant-time; do not derive `PartialEq` to avoid timing-unsafe
/// comparisons on the auth path.
#[derive(Debug, Clone)]
pub struct Capability([u8; 32]);

impl Capability {
    /// Generate a fresh random capability. Uses two v4 UUIDs (244 random bits).
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        Self(bytes)
    }

    /// Raw bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Constant-time equality — the only comparison used on the auth path.
    pub fn ct_eq(&self, other: &Capability) -> bool {
        let mut diff = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

impl Serialize for Capability {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64_URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bytes = BASE64_URL_SAFE_NO_PAD
            .decode(s.as_bytes())
            .map_err(D::Error::custom)?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| D::Error::custom("capability must be 32 bytes"))?;
        Ok(Self(arr))
    }
}

/// A shareable ticket: the minimum a consumer needs to *dial* the host. It is
/// deliberately small and constant-size — the manifest (which URLs, ports) is
/// **not** here; the host sends it over the tunnel after approval. This keeps
/// the ticket (and the join URL built from it) short no matter how many URLs a
/// run exposes. Serialized as URL-safe base64 JSON so it pastes as one token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShareTicket {
    /// iroh `EndpointTicket` string (NodeId + relay + direct addrs). Opaque to
    /// core; the daemon parses it to dial.
    pub iroh_ticket: String,
    /// Bearer capability (gate 1).
    pub capability: Capability,
}

// `Capability` has no `PartialEq`; derive it structurally for `ShareTicket`
// tests only, via byte comparison of the encoded form.
impl PartialEq for Capability {
    fn eq(&self, other: &Self) -> bool {
        self.ct_eq(other)
    }
}

impl ShareTicket {
    /// Encode to a single opaque, paste-safe token.
    pub fn encode(&self) -> Result<String, TicketError> {
        let json = serde_json::to_vec(self).map_err(TicketError::Serialize)?;
        Ok(format!("veldshare_{}", BASE64_URL_SAFE_NO_PAD.encode(json)))
    }

    /// Decode from the token produced by [`encode`](Self::encode).
    pub fn decode(s: &str) -> Result<Self, TicketError> {
        let b64 = s.strip_prefix("veldshare_").ok_or(TicketError::BadPrefix)?;
        let json = BASE64_URL_SAFE_NO_PAD
            .decode(b64.as_bytes())
            .map_err(TicketError::Base64)?;
        serde_json::from_slice(&json).map_err(TicketError::Deserialize)
    }
}

// ---------------------------------------------------------------------------
// Control-API DTOs (daemon HTTP on 127.0.0.1:19899, shared with the CLI client)
// ---------------------------------------------------------------------------

/// `POST /api/shares` — start sharing a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartShareRequest {
    /// Run name to share. `None` means "the only run", resolved by the daemon.
    pub run: Option<String>,
    /// Node names to share; `None` shares all of the run's URL-bearing nodes.
    pub nodes: Option<Vec<String>>,
    /// Lifetime in seconds; defaults applied by the daemon.
    pub ttl_secs: Option<i64>,
    /// Approval mode; defaults to the caller's context-appropriate mode.
    pub approve: Option<ApprovalMode>,
}

/// `POST /api/shares` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartShareResponse {
    pub share_id: String,
    /// The opaque `veldshare_…` token to hand to a colleague.
    pub ticket: String,
    /// Full browser join URL (built by the daemon from the setup mode).
    #[serde(default)]
    pub join_url: String,
    pub nodes: Vec<String>,
    pub expires_at: i64,
}

/// `POST /api/shares/join` — join a shared environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    /// The `veldshare_…` ticket.
    pub ticket: String,
    /// Untrusted self-label shown to the host.
    pub label: Option<String>,
}

/// `POST /api/shares/join` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub join_id: String,
    /// URLs now reachable locally on this machine.
    pub urls: Vec<String>,
    /// Non-fatal notes (e.g. nodes skipped because a local URL already owns the
    /// hostname — the local URL always wins).
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// One entry in `GET /api/shares`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareInfo {
    pub id: String,
    /// Run name this share exposes (empty for joins). Used to attach a hosted
    /// share to its run card in the dashboard.
    #[serde(default)]
    pub run: String,
    /// Approval mode of a hosted share (`first`/`manual`/`auto`).
    #[serde(default)]
    pub approve: Option<ApprovalMode>,
    pub nodes: Vec<String>,
    pub urls: Vec<String>,
    /// The join ticket (hosted shares only) so the dashboard can build the link.
    #[serde(default)]
    pub ticket: Option<String>,
    /// Full browser join URL (hosted shares only), built by the daemon so it's
    /// correct regardless of how the dashboard was opened.
    #[serde(default)]
    pub join_url: Option<String>,
    /// Number of consumers currently connected (hosted shares only).
    #[serde(default)]
    pub joiners: usize,
}

/// A join awaiting the host's approval (manual mode).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInfo {
    /// Request id, used to approve/deny.
    pub id: String,
    /// The share being joined.
    pub share_id: String,
    /// Untrusted self-label the joiner provided.
    pub label: String,
    /// The joiner's cryptographic identity (iroh node id) — the trusted one.
    pub node_id: String,
}

/// `GET /api/shares` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharesList {
    /// Shares this machine is hosting.
    pub shares: Vec<ShareInfo>,
    /// Shares this machine has joined.
    pub joins: Vec<ShareInfo>,
    /// Join requests awaiting this host's approval.
    pub pending: Vec<PendingInfo>,
}

/// Errors from ticket encoding/decoding.
#[derive(Debug, thiserror::Error)]
pub enum TicketError {
    #[error("ticket is missing the `veldshare_` prefix")]
    BadPrefix,
    #[error("ticket base64 is invalid: {0}")]
    Base64(base64::DecodeError),
    #[error("ticket JSON serialize failed: {0}")]
    Serialize(serde_json::Error),
    #[error("ticket JSON is invalid: {0}")]
    Deserialize(serde_json::Error),
}

// ---------------------------------------------------------------------------
// Daemon control-API client (used by the CLI)
// ---------------------------------------------------------------------------

/// Base URL of the daemon's local control API.
const DAEMON_BASE: &str = "http://127.0.0.1:19899";

/// Errors talking to the local daemon.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("the veld daemon is not running (run `veld setup` or start the service)")]
    NotRunning,
    #[error("daemon request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Api(String),
}

/// Thin HTTP client for the daemon's `/api/shares` control API.
pub struct DaemonClient {
    http: reqwest::Client,
    base: String,
}

impl Default for DaemonClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DaemonClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            base: DAEMON_BASE.to_string(),
        }
    }

    fn map_send(e: reqwest::Error) -> DaemonError {
        if e.is_connect() {
            DaemonError::NotRunning
        } else {
            DaemonError::Http(e)
        }
    }

    async fn parse<T: serde::de::DeserializeOwned>(
        resp: reqwest::Response,
    ) -> Result<T, DaemonError> {
        if resp.status().is_success() {
            resp.json::<T>().await.map_err(DaemonError::Http)
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(DaemonError::Api(format!("{status}: {body}")))
        }
    }

    async fn expect_empty(resp: reqwest::Response) -> Result<(), DaemonError> {
        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Err(DaemonError::Api(format!("{status}: {body}")))
        }
    }

    pub async fn start_share(
        &self,
        req: &StartShareRequest,
    ) -> Result<StartShareResponse, DaemonError> {
        let resp = self
            .http
            .post(format!("{}/api/shares", self.base))
            .header("X-Veld-Request", "1")
            .json(req)
            .send()
            .await
            .map_err(Self::map_send)?;
        Self::parse(resp).await
    }

    pub async fn join(&self, req: &JoinRequest) -> Result<JoinResponse, DaemonError> {
        let resp = self
            .http
            .post(format!("{}/api/shares/join", self.base))
            .header("X-Veld-Request", "1")
            .json(req)
            .send()
            .await
            .map_err(Self::map_send)?;
        Self::parse(resp).await
    }

    pub async fn list(&self) -> Result<SharesList, DaemonError> {
        let resp = self
            .http
            .get(format!("{}/api/shares", self.base))
            .send()
            .await
            .map_err(Self::map_send)?;
        Self::parse(resp).await
    }

    pub async fn unshare(&self, id: &str) -> Result<(), DaemonError> {
        let resp = self
            .http
            .delete(format!("{}/api/shares/{id}", self.base))
            .header("X-Veld-Request", "1")
            .send()
            .await
            .map_err(Self::map_send)?;
        Self::expect_empty(resp).await
    }

    pub async fn leave(&self, id: &str) -> Result<(), DaemonError> {
        let resp = self
            .http
            .delete(format!("{}/api/shares/joins/{id}", self.base))
            .header("X-Veld-Request", "1")
            .send()
            .await
            .map_err(Self::map_send)?;
        Self::expect_empty(resp).await
    }

    /// Stop all shares tied to a run (best-effort cleanup on `veld stop`).
    /// Returns how many shares were stopped.
    pub async fn unshare_run(&self, run_id: &str) -> Result<usize, DaemonError> {
        let resp = self
            .http
            .delete(format!("{}/api/shares/by-run/{run_id}", self.base))
            .header("X-Veld-Request", "1")
            .send()
            .await
            .map_err(Self::map_send)?;
        let v: serde_json::Value = Self::parse(resp).await?;
        Ok(v.get("unshared").and_then(|n| n.as_u64()).unwrap_or(0) as usize)
    }

    pub async fn approve(&self, req_id: &str) -> Result<(), DaemonError> {
        let resp = self
            .http
            .post(format!(
                "{}/api/shares/requests/{req_id}/approve",
                self.base
            ))
            .header("X-Veld-Request", "1")
            .send()
            .await
            .map_err(Self::map_send)?;
        Self::expect_empty(resp).await
    }

    pub async fn deny(&self, req_id: &str) -> Result<(), DaemonError> {
        let resp = self
            .http
            .post(format!("{}/api/shares/requests/{req_id}/deny", self.base))
            .header("X-Veld-Request", "1")
            .send()
            .await
            .map_err(Self::map_send)?;
        Self::expect_empty(resp).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> ShareManifest {
        ShareManifest {
            run_id: Uuid::new_v4(),
            run: "demo".to_string(),
            project: "irohtest".to_string(),
            nodes: vec![SharedNode {
                node: "app".to_string(),
                variant: "host".to_string(),
                hostname: "app.demo.irohtest.localhost".to_string(),
                url: "https://app.demo.irohtest.localhost".to_string(),
                upstream_port: 19001,
            }],
            created_at: 1_000_000,
            expires_at: 1_007_200,
        }
    }

    #[test]
    fn ticket_round_trips() {
        let ticket = ShareTicket {
            iroh_ticket: "endpointaaaa".to_string(),
            capability: Capability::generate(),
        };
        let encoded = ticket.encode().expect("encode");
        assert!(encoded.starts_with("veldshare_"));
        let decoded = ShareTicket::decode(&encoded).expect("decode");
        assert_eq!(ticket, decoded);
        // sample_manifest still constructs a valid manifest (sent over the wire).
        assert_eq!(sample_manifest().run, "demo");
    }

    #[test]
    fn decode_rejects_bad_prefix() {
        assert!(matches!(
            ShareTicket::decode("nope_xxxx"),
            Err(TicketError::BadPrefix)
        ));
    }

    #[test]
    fn capability_ct_eq() {
        let a = Capability::generate();
        let b = a.clone();
        let c = Capability::generate();
        assert!(a.ct_eq(&b));
        assert!(!a.ct_eq(&c));
    }

    #[test]
    fn capability_serializes_as_base64_string() {
        let cap = Capability::generate();
        let json = serde_json::to_string(&cap).expect("ser");
        // A JSON string, not an array of bytes.
        assert!(json.starts_with('"') && json.ends_with('"'));
        let back: Capability = serde_json::from_str(&json).expect("de");
        assert!(cap.ct_eq(&back));
    }

    #[test]
    fn approval_mode_serde_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&ApprovalMode::First).unwrap(),
            "\"first\""
        );
        assert_eq!(ApprovalMode::default(), ApprovalMode::First);
    }
}
