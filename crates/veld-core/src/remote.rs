//! Remote runner identity, pairing protocol, and private local persistence.
//!
//! The same OS user is trusted: that user can read the signing seed and replace
//! the state. Other users must not gain access through permissions or symlinks.
//! Callers must serialize access across **all** store handles and processes; this
//! module does not implement a process lock. Run disk operations on a blocking
//! pool, never an async worker. HTTP callers must bound request bodies before
//! deserialization, use TLS, and never log invitations, credentials, or state.
//!
//! Proof verification requires a pinned descriptor and a fresh client-generated
//! nonce for each connection. Consume that nonce once; verification alone does
//! not track replay. An invitation is a bearer capability and needs a trusted
//! transfer channel. The proposed remote port still needs integration reservation.

use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Current remote pairing and identity protocol.
pub const PROTOCOL_VERSION: u32 = 1;
/// Proposed remote listener port; integration must reserve it separately.
pub const DEFAULT_REMOTE_PORT: u16 = 19896;
/// Maximum JSON pairing or identity body accepted by the transport.
pub const MAX_WIRE_BYTES: usize = 16 * 1024;
const MAX_STATE_BYTES: usize = 1024 * 1024;
const MAX_ENDPOINT_BYTES: usize = 2048;
const MAX_LABEL_BYTES: usize = 128;
const MAX_VERSION_BYTES: usize = 128;
const MAX_CLIENTS: usize = 128;
const INVITATION_LIFETIME: i64 = 5 * 60;
const INVALID_ATTEMPT_LIMIT: u8 = 5;
const TOKEN_BYTES: usize = 32;
/// Exact length of a canonical nonce, invitation secret, or client credential.
pub const TOKEN_ENCODED_BYTES: usize = 43;
const INVITATION_PREFIX: &str = "veld-pair-v1:";
const STATE_FILE: &str = "state.json";
const STATE_SCHEMA: u32 = 1;
const STICKY_BIT: u32 = 0o1000;

/// An error never includes an input secret, credential, or private state value.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("invalid remote endpoint")]
    InvalidEndpoint,
    #[error("invalid remote descriptor")]
    InvalidDescriptor,
    #[error("invalid pairing invitation")]
    InvalidInvitation,
    #[error("invalid pairing request")]
    InvalidRequest,
    #[error("no active pairing invitation")]
    NoInvitation,
    #[error("pairing invitation expired")]
    InvitationExpired,
    #[error("pairing request rejected")]
    PairingRejected,
    #[error("remote client limit reached")]
    ClientLimit,
    #[error("invalid remote identity proof")]
    InvalidProof,
    #[error("remote store has unsafe ownership, permissions, or file type")]
    UnsafeStore,
    #[error("remote state is malformed or unsupported; it was not overwritten")]
    InvalidState,
    #[error("remote state changed; reopen the store before retrying")]
    StateChanged,
    #[error("remote state was replaced but directory sync failed; reopen the store: {0}")]
    Durability(std::io::Error),
    #[error("remote store requires reopening after an uncertain commit")]
    Unavailable,
    #[error("remote store I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Result used by the remote pairing core.
pub type Result<T> = std::result::Result<T, RemoteError>;

/// Production is always HTTPS on a `.ts.net` host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointPolicy {
    Production,
    /// Explicit test opt-in. HTTP is permitted only for literal loopback IPs.
    LoopbackTest,
}

/// Public runner identity. Pin the complete descriptor after trusted pairing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub protocol: u32,
    /// Canonical lowercase UUID v4.
    pub instance_id: String,
    /// Unpadded base64url encoding of the 32-byte Ed25519 public key.
    pub public_key: String,
    pub version: String,
    pub endpoint: String,
}

/// Short-lived bearer invitation. Deliberately does not implement `Debug`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairInvitation {
    pub descriptor: Descriptor,
    /// Canonical unpadded base64url token containing 32 bytes.
    pub secret: String,
    /// Unix timestamp in seconds; the runner checks this deadline on redemption.
    pub expires_at: i64,
}

impl PairInvitation {
    /// Encode a production invitation using unpadded base64url JSON.
    pub fn encode(&self) -> Result<String> {
        self.encode_with_policy(EndpointPolicy::Production)
    }

    /// Decode and validate a production invitation. The runner checks expiry.
    pub fn decode(encoded: &str) -> Result<Self> {
        Self::decode_with_policy(encoded, EndpointPolicy::Production)
    }

    /// Encode with an explicit endpoint policy, for isolated loopback tests.
    pub fn encode_with_policy(&self, policy: EndpointPolicy) -> Result<String> {
        self.validate(policy)?;
        let json = serde_json::to_vec(self).map_err(|_| RemoteError::InvalidInvitation)?;
        if json.len() > MAX_WIRE_BYTES {
            return Err(RemoteError::InvalidInvitation);
        }
        Ok(format!(
            "{INVITATION_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(json)
        ))
    }

    /// Decode with an explicit endpoint policy, for isolated loopback tests.
    pub fn decode_with_policy(encoded: &str, policy: EndpointPolicy) -> Result<Self> {
        if encoded.len() > INVITATION_PREFIX.len() + MAX_WIRE_BYTES * 4 / 3 + 4 {
            return Err(RemoteError::InvalidInvitation);
        }
        let payload = encoded
            .strip_prefix(INVITATION_PREFIX)
            .ok_or(RemoteError::InvalidInvitation)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| RemoteError::InvalidInvitation)?;
        if bytes.len() > MAX_WIRE_BYTES || URL_SAFE_NO_PAD.encode(&bytes) != payload {
            return Err(RemoteError::InvalidInvitation);
        }
        let invitation: Self =
            serde_json::from_slice(&bytes).map_err(|_| RemoteError::InvalidInvitation)?;
        invitation.validate(policy)?;
        Ok(invitation)
    }

    fn validate(&self, policy: EndpointPolicy) -> Result<()> {
        validate_descriptor(&self.descriptor, policy)
            .map_err(|_| RemoteError::InvalidInvitation)?;
        if decode_fixed::<TOKEN_BYTES>(&self.secret).is_none() || self.expires_at < 0 {
            return Err(RemoteError::InvalidInvitation);
        }
        Ok(())
    }
}

/// Pairing request. Transport code must enforce `MAX_WIRE_BYTES` before parsing.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairRequest {
    pub secret: String,
    pub label: String,
}

/// A credential is returned once and must never appear in diagnostics.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairResponse {
    pub client_id: String,
    /// Canonical unpadded base64url bearer token containing 32 bytes.
    pub credential: String,
    pub descriptor: Descriptor,
}

/// Nonsecret client metadata. IDs, rather than labels, identify clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClientInfo {
    pub id: String,
    pub label: String,
    /// Unix timestamp in seconds.
    pub created_at: i64,
}

/// Signature over a client challenge and the canonical runner origin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityProof {
    pub descriptor: Descriptor,
    /// Exact canonical 32-byte base64url challenge supplied by the client.
    pub nonce: String,
    /// Unpadded base64url encoding of the 64-byte Ed25519 signature.
    pub signature: String,
}

/// Generate a canonical 32-byte base64url challenge using OS-backed UUID v4.
pub fn generate_nonce() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes())
}

/// Canonicalize an origin. Paths other than `/` are rejected before URL parsing.
pub fn canonical_endpoint(endpoint: &str, policy: EndpointPolicy) -> Result<String> {
    if endpoint.len() > MAX_ENDPOINT_BYTES
        || !endpoint.is_ascii()
        || endpoint
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || endpoint.contains(['\\', '?', '#', '@', '%'])
    {
        return Err(RemoteError::InvalidEndpoint);
    }
    let (_, remainder) = endpoint
        .split_once("://")
        .ok_or(RemoteError::InvalidEndpoint)?;
    if let Some((_, path)) = remainder.split_once('/') {
        if !path.is_empty() {
            return Err(RemoteError::InvalidEndpoint);
        }
    }
    let url = reqwest::Url::parse(endpoint).map_err(|_| RemoteError::InvalidEndpoint)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || url.port() == Some(0)
    {
        return Err(RemoteError::InvalidEndpoint);
    }
    let host = url.host_str().ok_or(RemoteError::InvalidEndpoint)?;
    let tailscale = host.strip_suffix(".ts.net").is_some_and(|prefix| {
        !prefix.is_empty()
            && host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
    });
    let authority = remainder
        .split_once('/')
        .map_or(remainder, |(host, _)| host);
    // Check the literal input: URL parsing otherwise accepts shorthand such as
    // 127.1 or an integer IPv4 address and silently expands it to loopback.
    let loopback = authority
        .parse::<std::net::SocketAddr>()
        .map(|address| address.ip())
        .or_else(|_| {
            authority
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
        })
        .is_ok_and(|ip| ip.is_loopback());
    if !(url.scheme() == "https" && tailscale
        || policy == EndpointPolicy::LoopbackTest && url.scheme() == "http" && loopback)
    {
        return Err(RemoteError::InvalidEndpoint);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Exact signed bytes: `veld-remote-v1\n{instance_id}\n{origin}\n{nonce}`.
pub fn signing_message(instance_id: &str, endpoint: &str, nonce: &str) -> Result<Vec<u8>> {
    signing_message_with_policy(instance_id, endpoint, nonce, EndpointPolicy::Production)
}

/// Build signed bytes with the explicit test policy when using loopback HTTP.
pub fn signing_message_with_policy(
    instance_id: &str,
    endpoint: &str,
    nonce: &str,
    policy: EndpointPolicy,
) -> Result<Vec<u8>> {
    if !valid_id(instance_id) || decode_fixed::<TOKEN_BYTES>(nonce).is_none() {
        return Err(RemoteError::InvalidProof);
    }
    let origin = canonical_endpoint(endpoint, policy)?;
    Ok(format!("veld-remote-v1\n{instance_id}\n{origin}\n{nonce}").into_bytes())
}

/// Verify a production proof against a pinned descriptor and a fresh nonce.
pub fn verify_proof(proof: &IdentityProof, expected: &Descriptor, nonce: &str) -> Result<()> {
    verify_proof_with_policy(proof, expected, nonce, EndpointPolicy::Production)
}

/// Verify with an explicit endpoint policy, for isolated loopback tests.
pub fn verify_proof_with_policy(
    proof: &IdentityProof,
    expected: &Descriptor,
    nonce: &str,
    policy: EndpointPolicy,
) -> Result<()> {
    validate_descriptor(expected, policy).map_err(|_| RemoteError::InvalidProof)?;
    validate_descriptor(&proof.descriptor, policy).map_err(|_| RemoteError::InvalidProof)?;
    if proof.descriptor.protocol != expected.protocol
        || proof.descriptor.instance_id != expected.instance_id
        || proof.descriptor.public_key != expected.public_key
        || proof.descriptor.endpoint != expected.endpoint
        || proof.nonce != nonce
    {
        return Err(RemoteError::InvalidProof);
    }
    let key_bytes = decode_fixed::<32>(&expected.public_key).ok_or(RemoteError::InvalidProof)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| RemoteError::InvalidProof)?;
    let signature = decode_fixed::<64>(&proof.signature).ok_or(RemoteError::InvalidProof)?;
    let message =
        signing_message_with_policy(&expected.instance_id, &expected.endpoint, nonce, policy)?;
    key.verify_strict(&message, &Signature::from_bytes(&signature))
        .map_err(|_| RemoteError::InvalidProof)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredClient {
    info: ClientInfo,
    verifier: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredState {
    schema: u32,
    instance_id: String,
    signing_seed: String,
    clients: Vec<StoredClient>,
}

struct PendingInvitation {
    verifier: [u8; 32],
    issued_at: i64,
    expires_at: i64,
    invalid_attempts: u8,
}

/// Owner-only runner store. Keep a single serialized owner per directory.
///
/// Opening never resets malformed state or changes existing permissions. A
/// failed write before replacement leaves both memory and the invitation intact.
/// A sync failure after replacement is an uncertain commit: the new state is
/// installed in memory, the invitation is consumed, and all access fails closed
/// until reopen. Recovery must not assume the operation was rolled back.
pub struct RunnerStore {
    directory: PathBuf,
    directory_file: File,
    state: StoredState,
    disk_digest: Option<[u8; 32]>,
    signing_key: SigningKey,
    descriptor: Descriptor,
    policy: EndpointPolicy,
    pending: Option<PendingInvitation>,
    unavailable: bool,
}

impl RunnerStore {
    /// Open or create a production store with owner-only directory and state.
    pub fn open(directory: &Path, endpoint: &str) -> Result<Self> {
        Self::open_with_policy(directory, endpoint, EndpointPolicy::Production)
    }

    /// Explicit endpoint-policy override for isolated loopback integration tests.
    pub fn open_with_policy(
        directory: &Path,
        endpoint: &str,
        policy: EndpointPolicy,
    ) -> Result<Self> {
        let endpoint = canonical_endpoint(endpoint, policy)?;
        let (directory, directory_file) = secure_directory(directory)?;
        let bytes = read_state(&directory_file)?;
        let state = match bytes.as_ref() {
            Some(bytes) => parse_state(bytes)?,
            None => StoredState {
                schema: STATE_SCHEMA,
                instance_id: Uuid::new_v4().to_string(),
                signing_seed: URL_SAFE_NO_PAD.encode(random_bytes()),
                clients: Vec::new(),
            },
        };
        let seed = decode_fixed::<32>(&state.signing_seed).ok_or(RemoteError::InvalidState)?;
        let signing_key = SigningKey::from_bytes(&seed);
        let descriptor = Descriptor {
            protocol: PROTOCOL_VERSION,
            instance_id: state.instance_id.clone(),
            public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
            version: crate::VERSION.to_owned(),
            endpoint,
        };
        validate_descriptor(&descriptor, policy)?;
        let mut store = Self {
            directory,
            directory_file,
            disk_digest: bytes.as_ref().map(|bytes| hash(bytes)),
            state,
            signing_key,
            descriptor,
            policy,
            pending: None,
            unavailable: false,
        };
        if bytes.is_none() {
            store.commit(store.state.clone())?;
        }
        Ok(store)
    }

    /// Public identity. The endpoint always has canonical origin form.
    pub fn descriptor(&self) -> Descriptor {
        self.descriptor.clone()
    }

    /// Sign a canonical challenge. No disk I/O is performed.
    pub fn prove(&self, nonce: &str) -> Result<IdentityProof> {
        self.ensure_available()?;
        let message = signing_message_with_policy(
            &self.descriptor.instance_id,
            &self.descriptor.endpoint,
            nonce,
            self.policy,
        )?;
        Ok(IdentityProof {
            descriptor: self.descriptor(),
            nonce: nonce.to_owned(),
            signature: URL_SAFE_NO_PAD.encode(self.signing_key.sign(&message).to_bytes()),
        })
    }

    /// Replace any outstanding invitation with a five-minute memory-only one.
    pub fn invite(&mut self, now: i64) -> Result<PairInvitation> {
        self.ensure_available()?;
        let expires_at = now
            .checked_add(INVITATION_LIFETIME)
            .filter(|_| now >= 0)
            .ok_or(RemoteError::InvalidRequest)?;
        let secret = generate_nonce();
        self.pending = Some(PendingInvitation {
            verifier: verifier(b"veld-pair-v1", &secret),
            issued_at: now,
            expires_at,
            invalid_attempts: 0,
        });
        Ok(PairInvitation {
            descriptor: self.descriptor(),
            secret,
            expires_at,
        })
    }

    /// Consume one invitation only after the client is atomically persisted.
    /// Duplicate labels are permitted; callers revoke by the unique client ID.
    pub fn redeem(&mut self, request: PairRequest, now: i64) -> Result<PairResponse> {
        self.ensure_available()?;
        let pending = self.pending.as_ref().ok_or(RemoteError::NoInvitation)?;
        if now < pending.issued_at || now >= pending.expires_at {
            self.pending = None;
            return Err(RemoteError::InvitationExpired);
        }
        let valid_secret = decode_fixed::<TOKEN_BYTES>(&request.secret).is_some()
            && constant_time_eq(
                &pending.verifier,
                &verifier(b"veld-pair-v1", &request.secret),
            );
        if !valid_secret || !valid_label(&request.label) {
            self.reject_attempt();
            return Err(RemoteError::PairingRejected);
        }
        if self.state.clients.len() >= MAX_CLIENTS {
            return Err(RemoteError::ClientLimit);
        }
        let credential = generate_nonce();
        let client_id = Uuid::new_v4().to_string();
        let mut next = self.state.clone();
        next.clients.push(StoredClient {
            info: ClientInfo {
                id: client_id.clone(),
                label: request.label,
                created_at: now,
            },
            verifier: URL_SAFE_NO_PAD.encode(verifier(b"veld-client-v1", &credential)),
        });
        self.commit(next)?;
        self.pending = None;
        Ok(PairResponse {
            client_id,
            credential,
            descriptor: self.descriptor(),
        })
    }

    /// Authenticate without disk I/O. Only a SHA-256 verifier is retained.
    pub fn authenticate(&self, credential: &str) -> Option<String> {
        if self.unavailable || decode_fixed::<TOKEN_BYTES>(credential).is_none() {
            return None;
        }
        let supplied = verifier(b"veld-client-v1", credential);
        let mut matched = None;
        for client in &self.state.clients {
            // All verifiers were validated when the state was loaded or written.
            let stored = decode_fixed::<32>(&client.verifier)?;
            if constant_time_eq(&stored, &supplied) {
                matched = Some(client.info.id.clone());
            }
        }
        matched
    }

    /// Return nonsecret metadata. An unavailable store exposes no clients.
    pub fn clients(&self) -> Vec<ClientInfo> {
        if self.unavailable {
            return Vec::new();
        }
        self.state
            .clients
            .iter()
            .map(|client| client.info.clone())
            .collect()
    }

    /// Persist revocation before returning true. An unknown ID returns false.
    pub fn revoke(&mut self, id: &str) -> Result<bool> {
        self.ensure_available()?;
        if !valid_id(id) {
            return Err(RemoteError::InvalidRequest);
        }
        if !self.state.clients.iter().any(|client| client.info.id == id) {
            return Ok(false);
        }
        let mut next = self.state.clone();
        next.clients.retain(|client| client.info.id != id);
        self.commit(next)?;
        Ok(true)
    }

    fn ensure_available(&self) -> Result<()> {
        if self.unavailable {
            Err(RemoteError::Unavailable)
        } else {
            Ok(())
        }
    }

    fn reject_attempt(&mut self) {
        if let Some(pending) = self.pending.as_mut() {
            pending.invalid_attempts += 1;
            if pending.invalid_attempts >= INVALID_ATTEMPT_LIMIT {
                self.pending = None;
            }
        }
    }

    fn check_disk(&self) -> Result<()> {
        validate_metadata(&self.directory_file.metadata()?, true)?;
        let current = read_state(&self.directory_file)?;
        if let Some(bytes) = current.as_ref() {
            parse_state(bytes)?;
        }
        if current.as_ref().map(|bytes| hash(bytes)) != self.disk_digest {
            return Err(RemoteError::StateChanged);
        }
        Ok(())
    }

    fn commit(&mut self, next: StoredState) -> Result<()> {
        self.check_disk()?;
        let bytes = serde_json::to_vec(&next).map_err(|_| RemoteError::InvalidState)?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(RemoteError::InvalidState);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        self.check_disk()?;
        let path = self.directory.join(STATE_FILE);
        if self.disk_digest.is_none() {
            temporary
                .persist_noclobber(&path)
                .map_err(|error| RemoteError::Io(error.error))?;
        } else {
            temporary
                .persist(&path)
                .map_err(|error| RemoteError::Io(error.error))?;
        }
        self.disk_digest = Some(hash(&bytes));
        self.state = next;
        if let Err(error) = self.directory_file.sync_all() {
            self.pending = None;
            self.unavailable = true;
            return Err(RemoteError::Durability(error));
        }
        Ok(())
    }
}

fn validate_descriptor(descriptor: &Descriptor, policy: EndpointPolicy) -> Result<()> {
    let key = decode_fixed::<32>(&descriptor.public_key).ok_or(RemoteError::InvalidDescriptor)?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| RemoteError::InvalidDescriptor)?;
    if descriptor.protocol != PROTOCOL_VERSION
        || !valid_id(&descriptor.instance_id)
        || !valid_text(&descriptor.version, MAX_VERSION_BYTES)
        || key.is_weak()
        || canonical_endpoint(&descriptor.endpoint, policy)? != descriptor.endpoint
    {
        return Err(RemoteError::InvalidDescriptor);
    }
    Ok(())
}

fn valid_id(value: &str) -> bool {
    value.len() == 36
        && Uuid::parse_str(value)
            .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == value)
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_label(value: &str) -> bool {
    valid_text(value, MAX_LABEL_BYTES)
}

fn decode_fixed<const N: usize>(value: &str) -> Option<[u8; N]> {
    if value.len() != (N * 8).div_ceil(6) {
        return None;
    }
    let bytes: [u8; N] = URL_SAFE_NO_PAD.decode(value).ok()?.try_into().ok()?;
    (URL_SAFE_NO_PAD.encode(bytes) == value).then_some(bytes)
}

fn random_bytes() -> [u8; TOKEN_BYTES] {
    // Two UUID v4 values contain 244 OS-random bits, above the 128-bit minimum.
    let mut bytes = [0; TOKEN_BYTES];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn verifier(domain: &[u8], token: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(b"\0");
    digest.update(token.as_bytes());
    digest.finalize().into()
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    // Volatile reads keep the fixed-length comparison from becoming memcmp.
    let mut difference = 0u8;
    for (left_byte, right_byte) in left.iter().zip(right) {
        // SAFETY: both references remain live and point to initialized bytes.
        let (a, b) = unsafe {
            (
                std::ptr::read_volatile(left_byte),
                std::ptr::read_volatile(right_byte),
            )
        };
        difference |= a ^ b;
    }
    difference == 0
}

fn parse_state(bytes: &[u8]) -> Result<StoredState> {
    let state: StoredState =
        serde_json::from_slice(bytes).map_err(|_| RemoteError::InvalidState)?;
    if state.schema != STATE_SCHEMA
        || !valid_id(&state.instance_id)
        || decode_fixed::<32>(&state.signing_seed).is_none()
        || state.clients.len() > MAX_CLIENTS
    {
        return Err(RemoteError::InvalidState);
    }
    let mut ids = HashSet::new();
    let mut verifiers = HashSet::new();
    for client in &state.clients {
        if !valid_id(&client.info.id)
            || !valid_label(&client.info.label)
            || client.info.created_at < 0
            || decode_fixed::<32>(&client.verifier).is_none()
            || !ids.insert(&client.info.id)
            || !verifiers.insert(&client.verifier)
        {
            return Err(RemoteError::InvalidState);
        }
    }
    Ok(state)
}

fn validate_metadata(metadata: &fs::Metadata, directory: bool) -> Result<()> {
    // SAFETY: geteuid has no preconditions or side effects.
    let owner = unsafe { libc::geteuid() };
    let expected_mode = if directory { 0o700 } else { 0o600 };
    if metadata.uid() != owner
        || metadata.mode() & 0o7777 != expected_mode
        || (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(RemoteError::UnsafeStore);
    }
    Ok(())
}

fn secure_directory(path: &Path) -> Result<(PathBuf, File)> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut directory = File::open("/")?;
    let components: Vec<_> = absolute.components().collect();
    if components.len() < 2 {
        return Err(RemoteError::UnsafeStore);
    }
    for component in components {
        let name = match component {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => {
                CString::new(name.as_encoded_bytes()).map_err(|_| RemoteError::UnsafeStore)?
            }
            _ => return Err(RemoteError::UnsafeStore),
        };
        directory = open_directory_at(&directory, &name)?;
        let metadata = directory.metadata()?;
        // Ancestors can be root-owned. Sticky directories such as /tmp prevent
        // other users from replacing our entry even though the directory is shared.
        // SAFETY: geteuid has no preconditions or side effects.
        let owner = unsafe { libc::geteuid() };
        if (metadata.uid() != owner && metadata.uid() != 0)
            || (metadata.mode() & 0o022 != 0 && metadata.mode() & STICKY_BIT == 0)
        {
            return Err(RemoteError::UnsafeStore);
        }
    }
    validate_metadata(&directory.metadata()?, true)?;
    Ok((absolute, directory))
}

fn open_directory_at(parent: &File, name: &CString) -> Result<File> {
    let open = || {
        // SAFETY: parent is live and name is NUL-terminated.
        unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        }
    };
    let mut fd = open();
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
        // SAFETY: parent is live and name is NUL-terminated.
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        } else {
            parent.sync_all()?;
        }
        fd = open();
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn read_state(directory: &File) -> Result<Option<Vec<u8>>> {
    let name = CString::new(STATE_FILE).map_err(|_| RemoteError::InvalidState)?;
    // SAFETY: directory is live and name is NUL-terminated. NONBLOCK ensures a
    // planted FIFO cannot block before the regular-file check rejects it.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error.into())
        };
    }
    // SAFETY: openat returned a new owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    validate_metadata(&metadata, false)?;
    if metadata.len() > MAX_STATE_BYTES as u64 {
        return Err(RemoteError::InvalidState);
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(RemoteError::InvalidState);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const ENDPOINT: &str = "https://runner.example.ts.net:19896";
    const NOW: i64 = 1_800_000_000;

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        // macOS commonly exposes its temporary directory through /var, a symlink.
        let directory = temporary.path().canonicalize().unwrap().join("remote");
        (temporary, directory)
    }

    fn pair(store: &mut RunnerStore) -> PairResponse {
        let invitation = store.invite(NOW).unwrap();
        store
            .redeem(
                PairRequest {
                    secret: invitation.secret,
                    label: "Laptop".into(),
                },
                NOW,
            )
            .unwrap()
    }

    #[test]
    fn software_updates_preserve_pinned_identity() {
        let (_temporary, directory) = scratch();
        let store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let mut pinned = store.descriptor();
        pinned.version = "older-version".into();
        let nonce = generate_nonce();
        verify_proof(&store.prove(&nonce).unwrap(), &pinned, &nonce).unwrap();
        pinned.protocol += 1;
        assert!(verify_proof(&store.prove(&nonce).unwrap(), &pinned, &nonce).is_err());
    }

    #[test]
    fn pairing_roundtrip_authentication_and_revocation() {
        let (_temporary, directory) = scratch();
        let mut store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let invitation = store.invite(NOW).unwrap();
        let encoded = invitation.encode().unwrap();
        let decoded = PairInvitation::decode(&encoded).unwrap();
        assert_eq!(decoded.descriptor, store.descriptor());
        assert_eq!(decoded.secret, invitation.secret);
        assert_eq!(decoded.expires_at, NOW + INVITATION_LIFETIME);
        let response = store
            .redeem(
                PairRequest {
                    secret: decoded.secret,
                    label: "Laptop".into(),
                },
                NOW,
            )
            .unwrap();
        assert_eq!(
            store.authenticate(&response.credential),
            Some(response.client_id.clone())
        );
        assert!(store.authenticate(&generate_nonce()).is_none());
        assert_eq!(store.clients()[0].label, "Laptop");
        let disk = fs::read_to_string(directory.join(STATE_FILE)).unwrap();
        assert!(!disk.contains(&response.credential));
        assert!(!disk.contains(&invitation.secret));
        assert!(store.revoke(&response.client_id).unwrap());
        assert!(!store.revoke(&response.client_id).unwrap());
        assert!(store.authenticate(&response.credential).is_none());
        drop(store);
        assert!(
            RunnerStore::open(&directory, ENDPOINT)
                .unwrap()
                .clients()
                .is_empty()
        );
    }

    #[test]
    fn reload_preserves_identity_and_clients_but_clears_invitation() {
        let (_temporary, directory) = scratch();
        let mut store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let response = pair(&mut store);
        let descriptor = store.descriptor();
        let invitation = store.invite(NOW).unwrap();
        drop(store);
        let mut store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        assert_eq!(store.descriptor(), descriptor);
        assert_eq!(
            store.authenticate(&response.credential),
            Some(response.client_id)
        );
        assert!(matches!(
            store.redeem(
                PairRequest {
                    secret: invitation.secret,
                    label: "Other".into()
                },
                NOW
            ),
            Err(RemoteError::NoInvitation)
        ));
        verify_proof(
            &store.prove(&generate_nonce()).unwrap(),
            &descriptor,
            &generate_nonce(),
        )
        .unwrap_err();
        let nonce = generate_nonce();
        verify_proof(&store.prove(&nonce).unwrap(), &descriptor, &nonce).unwrap();
    }

    #[test]
    fn expiration_replay_and_replacement_are_rejected() {
        let (_temporary, directory) = scratch();
        let mut store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let old = store.invite(NOW).unwrap();
        let current = store.invite(NOW).unwrap();
        assert!(matches!(
            store.redeem(
                PairRequest {
                    secret: old.secret,
                    label: "Old".into()
                },
                NOW
            ),
            Err(RemoteError::PairingRejected)
        ));
        let request = || PairRequest {
            secret: current.secret.clone(),
            label: "Current".into(),
        };
        store.redeem(request(), NOW).unwrap();
        assert!(matches!(
            store.redeem(request(), NOW),
            Err(RemoteError::NoInvitation)
        ));
        let expired = store.invite(NOW).unwrap();
        assert!(matches!(
            store.redeem(
                PairRequest {
                    secret: expired.secret,
                    label: "Late".into()
                },
                expired.expires_at
            ),
            Err(RemoteError::InvitationExpired)
        ));
        let rollback = store.invite(NOW).unwrap();
        assert!(matches!(
            store.redeem(
                PairRequest {
                    secret: rollback.secret,
                    label: "Early".into()
                },
                NOW - 1
            ),
            Err(RemoteError::InvitationExpired)
        ));
    }

    #[test]
    fn fifth_invalid_attempt_cancels_invitation() {
        let (_temporary, directory) = scratch();
        let mut store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let invitation = store.invite(NOW).unwrap();
        for _ in 0..INVALID_ATTEMPT_LIMIT {
            assert!(matches!(
                store.redeem(
                    PairRequest {
                        secret: "wrong".into(),
                        label: "Client".into()
                    },
                    NOW
                ),
                Err(RemoteError::PairingRejected)
            ));
        }
        assert!(matches!(
            store.redeem(
                PairRequest {
                    secret: invitation.secret,
                    label: "Client".into()
                },
                NOW
            ),
            Err(RemoteError::NoInvitation)
        ));
        assert!(store.clients().is_empty());
    }

    #[test]
    fn proofs_bind_challenge_origin_and_identity() {
        let (_temporary, directory) = scratch();
        let store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let descriptor = store.descriptor();
        let nonce = generate_nonce();
        let proof = store.prove(&nonce).unwrap();
        verify_proof(&proof, &descriptor, &nonce).unwrap();
        assert!(verify_proof(&proof, &descriptor, &generate_nonce()).is_err());
        assert!(store.prove("unbounded-or-noncanonical").is_err());
        assert!(store.prove(&format!("{nonce}=")).is_err());
        let mut changed = proof.clone();
        changed.descriptor.endpoint = "https://other.example.ts.net".into();
        assert!(verify_proof(&changed, &changed.descriptor, &nonce).is_err());
        changed = proof.clone();
        let other_key = SigningKey::from_bytes(&random_bytes());
        changed.descriptor.public_key =
            URL_SAFE_NO_PAD.encode(other_key.verifying_key().to_bytes());
        assert!(verify_proof(&changed, &changed.descriptor, &nonce).is_err());
        changed = proof.clone();
        changed.descriptor.instance_id = Uuid::new_v4().to_string();
        assert!(verify_proof(&changed, &changed.descriptor, &nonce).is_err());
        let message =
            signing_message(&descriptor.instance_id, &format!("{ENDPOINT}/"), &nonce).unwrap();
        assert_eq!(
            message,
            format!(
                "veld-remote-v1\n{}\n{ENDPOINT}\n{nonce}",
                descriptor.instance_id
            )
            .as_bytes()
        );
    }

    #[test]
    fn endpoint_policy_rejects_unsafe_origins() {
        for endpoint in [
            "http://host.ts.net",
            "https://example.com",
            "https://ts.net",
            "https://badts.net",
            "https://a.ts.net/path",
            "https://a.ts.net/../",
            "https://a.ts.net?",
            "https://a.ts.net#",
            "https://user@a.ts.net",
            "https://a.ts.net:0",
            "https://a.ts.net\\evil",
            "http://localhost:19896",
            "http://127.0.0.1:19896",
        ] {
            assert!(
                canonical_endpoint(endpoint, EndpointPolicy::Production).is_err(),
                "{endpoint}"
            );
        }
        assert_eq!(
            canonical_endpoint("https://RUNNER.ts.net:443/", EndpointPolicy::Production).unwrap(),
            "https://runner.ts.net"
        );
        for endpoint in ["http://127.0.0.1:19896", "http://[::1]:19896"] {
            let (_temporary, directory) = scratch();
            let store =
                RunnerStore::open_with_policy(&directory, endpoint, EndpointPolicy::LoopbackTest)
                    .unwrap();
            let nonce = generate_nonce();
            verify_proof_with_policy(
                &store.prove(&nonce).unwrap(),
                &store.descriptor(),
                &nonce,
                EndpointPolicy::LoopbackTest,
            )
            .unwrap();
            assert!(
                verify_proof(&store.prove(&nonce).unwrap(), &store.descriptor(), &nonce).is_err()
            );
        }
        for endpoint in [
            "http://192.0.2.1",
            "http://127.1",
            "http://2130706433",
            "http://localhost",
        ] {
            assert!(canonical_endpoint(endpoint, EndpointPolicy::LoopbackTest).is_err());
        }
    }

    #[test]
    fn corrupt_state_is_never_overwritten_and_failed_redeem_rolls_back() {
        let (_temporary, directory) = scratch();
        let mut store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let invitation = store.invite(NOW).unwrap();
        let original = fs::read(directory.join(STATE_FILE)).unwrap();
        fs::write(directory.join(STATE_FILE), b"{broken").unwrap();
        assert!(matches!(
            store.redeem(
                PairRequest {
                    secret: invitation.secret.clone(),
                    label: "Laptop".into()
                },
                NOW
            ),
            Err(RemoteError::InvalidState)
        ));
        assert!(store.clients().is_empty());
        assert!(matches!(
            RunnerStore::open(&directory, ENDPOINT),
            Err(RemoteError::InvalidState)
        ));
        assert_eq!(fs::read(directory.join(STATE_FILE)).unwrap(), b"{broken");
        fs::write(directory.join(STATE_FILE), original).unwrap();
        store
            .redeem(
                PairRequest {
                    secret: invitation.secret,
                    label: "Laptop".into(),
                },
                NOW,
            )
            .unwrap();
    }

    #[test]
    fn failed_revocation_retains_client_and_detects_concurrent_writer() {
        let (_temporary, directory) = scratch();
        let mut store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let response = pair(&mut store);
        let mut other = RunnerStore::open(&directory, ENDPOINT).unwrap();
        let another = pair(&mut other);
        assert!(matches!(
            store.revoke(&response.client_id),
            Err(RemoteError::StateChanged)
        ));
        assert!(store.authenticate(&response.credential).is_some());
        let reloaded = RunnerStore::open(&directory, ENDPOINT).unwrap();
        assert!(reloaded.authenticate(&another.credential).is_some());
    }

    #[test]
    fn unix_permissions_symlinks_and_hardlinks_are_enforced() {
        let (_temporary, directory) = scratch();
        let store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        assert_eq!(fs::metadata(&directory).unwrap().mode() & 0o7777, 0o700);
        let path = directory.join(STATE_FILE);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        drop(store);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            RunnerStore::open(&directory, ENDPOINT),
            Err(RemoteError::UnsafeStore)
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, directory.join("duplicate")).unwrap();
        assert!(matches!(
            RunnerStore::open(&directory, ENDPOINT),
            Err(RemoteError::UnsafeStore)
        ));
        fs::remove_file(directory.join("duplicate")).unwrap();
        let moved = directory.join("saved");
        fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        assert!(RunnerStore::open(&directory, ENDPOINT).is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let link = directory.with_file_name("remote-link");
        symlink(&directory, &link).unwrap();
        assert!(RunnerStore::open(&link, ENDPOINT).is_err());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RunnerStore::open(&directory, ENDPOINT).is_err());
    }

    #[test]
    fn invitation_and_request_inputs_are_bounded() {
        let (_temporary, directory) = scratch();
        let mut store = RunnerStore::open(&directory, ENDPOINT).unwrap();
        assert!(PairInvitation::decode(&"x".repeat(MAX_WIRE_BYTES * 2)).is_err());
        assert!(PairInvitation::decode("veld-pair-v2:e30").is_err());
        let invitation = store.invite(NOW).unwrap();
        assert!(PairInvitation::decode(&format!("{}=", invitation.encode().unwrap())).is_err());
        for label in [
            String::new(),
            " ".into(),
            "bad\nlabel".into(),
            "x".repeat(MAX_LABEL_BYTES + 1),
        ] {
            assert!(matches!(
                store.redeem(
                    PairRequest {
                        secret: invitation.secret.clone(),
                        label
                    },
                    NOW
                ),
                Err(RemoteError::PairingRejected)
            ));
        }
        assert!(store.clients().is_empty());
        assert!(store.invite(i64::MAX).is_err());
        assert!(store.invite(-1).is_err());
        assert_eq!(generate_nonce().len(), TOKEN_ENCODED_BYTES);
    }
}
