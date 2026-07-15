use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not find veld.json in {0} or any parent directory")]
    NotFound(PathBuf),

    #[error("failed to read veld.json at {path}: {source}")]
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse veld.json at {path}: {source}")]
    ParseError {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[error("unsupported schema version \"{0}\" — run `veld update` to get the latest version")]
    UnsupportedSchemaVersion(String),
}

// ---------------------------------------------------------------------------
// Top-level config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VeldConfig {
    /// Optional JSON-schema pointer for editor autocompletion.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,

    /// Must be "1" for v1.
    #[serde(rename = "schemaVersion")]
    pub schema_version: String,

    /// Human-readable project name.
    pub name: String,

    /// URL template with `{service}`, `{run}`, `{project}`, etc.
    #[serde(default = "default_url_template")]
    pub url_template: String,

    /// Named shortcuts for node:variant selections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<HashMap<String, Vec<String>>>,

    /// Client-side log levels to capture (project-level default).
    /// Supported values: "log", "warn", "error", "info", "debug".
    /// "exception" is always captured regardless of this setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_log_levels: Option<Vec<String>>,

    /// Feature toggles (project-level defaults).
    /// Overridable at node and variant level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesConfig>,

    /// Global environment variables inherited by all node variants.
    /// Overridable at node and variant level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,

    /// Environment sharing policy: which relays to use, and where the public
    /// web gateway lives. Per-service opt-in lives on each variant (`share`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing: Option<SharingConfig>,

    /// Setup steps that run sequentially before the dependency graph.
    /// If any step exits non-zero, startup is aborted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<Vec<SetupStep>>,

    /// Teardown steps that run sequentially after all nodes stop.
    /// Best-effort: failures are logged but do not block the stop operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown: Option<Vec<SetupStep>>,

    /// The dependency graph nodes.
    pub nodes: HashMap<String, NodeConfig>,
}

// ---------------------------------------------------------------------------
// Setup / Teardown steps
// ---------------------------------------------------------------------------

/// A lightweight step that runs before the dependency graph (setup) or after
/// all nodes stop (teardown). Not a node — no variants, no health checks,
/// no dependency graph participation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStep {
    /// Human-readable name for progress reporting.
    pub name: String,

    /// Shell command to execute.
    pub command: String,

    /// Optional message shown when the command fails (non-zero exit).
    /// Primarily useful for setup steps that validate prerequisites.
    #[serde(
        rename = "failureMessage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub failure_message: Option<String>,
}

fn default_url_template() -> String {
    "{service}.{run}.{project}.localhost".to_owned()
}

// ---------------------------------------------------------------------------
// Sharing
// ---------------------------------------------------------------------------

/// Environment-wide sharing policy. Relay selection is a compliance control:
/// `public` uses n0's public relays; a custom list confines share traffic to
/// relays the operator runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharingConfig {
    /// Relay policy. Relays must be opted into explicitly — including public —
    /// so nothing is routed over public relays by accident. Absent means "unset":
    /// the daemon then falls back to the `VELD_SHARE_RELAY` env override, and if
    /// that is also unset, `veld share` is refused. When set, config wins over
    /// the env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relays: Option<RelayPolicy>,

    /// Base URL of the public web gateway this environment points at. Only
    /// needed for services that `expose` `web`. Example:
    /// `https://share.acme.internal`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,

    /// **DANGER.** When true, the resolved relay auth token(s) are embedded in
    /// the share ticket, so a joiner needs no out-of-band token config. This
    /// puts the relay secret into every share link (Slack, email, browser
    /// history, …), defeating the token's purpose for any shared or long-lived
    /// relay secret. Enable **only** for disposable, per-project tokens you
    /// rotate freely — never a shared org relay secret. Off by default; the
    /// join side otherwise prompts for the token and caches it locally.
    ///
    /// Named à la React's `dangerouslySetInnerHTML` to force a deliberate
    /// choice; hence the camelCase JSON key, which stands out against veld's
    /// otherwise snake_case config.
    #[serde(
        default,
        rename = "dangerouslyEmbedRelayTokensInTicket",
        skip_serializing_if = "is_false"
    )]
    pub dangerously_embed_relay_tokens_in_ticket: bool,
}

/// `skip_serializing_if` predicate: omit a `bool` field when it is `false`.
fn is_false(b: &bool) -> bool {
    !b
}

/// Which iroh relays to route share traffic through. Serializes as either the
/// string `"public"` or an array of relay entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayPolicy {
    /// n0's public relay set (only via an explicit `"public"` opt-in).
    Public,
    /// Self-hosted relays. Share traffic is confined to these.
    Custom(Vec<RelayEntry>),
}

/// A single self-hosted relay in a [`RelayPolicy::Custom`] list.
///
/// A relay may require an authorization token (iroh sends it as an
/// `Authorization: Bearer <token>` header on the relay connection) so that only
/// authorized clients can use it — a cheap gate that keeps a self-hosted relay
/// from being an open one. The token is resolved at share time from its
/// [`SecretSource`]; it is never persisted in resolved form.
///
/// Serializes as a bare URL string when no token is set (round-tripping the
/// pre-token config form), or as `{ "url": ..., "token": ... }` when it is.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RelayEntry {
    /// The relay URL (e.g. `https://relay.acme.internal`).
    pub url: String,
    /// Optional authorization token source. `None` = the relay is open / needs
    /// no auth.
    pub token: Option<SecretSource>,
}

impl RelayEntry {
    /// A relay entry with no authorization token (an open relay).
    pub fn url(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: None,
        }
    }
}

impl std::fmt::Debug for RelayEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Delegates the token field to `SecretSource`'s redacting Debug.
        f.debug_struct("RelayEntry")
            .field("url", &self.url)
            .field("token", &self.token)
            .finish()
    }
}

/// Where a secret (currently only relay auth tokens) is read from at use time.
///
/// A plain string in config is a [`SecretSource::Literal`] — convenient for
/// local dev, but it lands the secret in `veld.json` (and version control).
/// The object forms keep the secret *out* of config and are preferred for real
/// deployments:
///
/// - `{ "env": "VAR" }` — read from the daemon's environment (12-factor).
/// - `{ "file": "/path" }` — read from a file (Docker/Kubernetes secret mounts).
/// - `{ "command": "op read op://vault/relay/token" }` — run a shell command and
///   use its stdout (1Password / Vault / any secret-manager CLI).
///
/// Resolution (running the command, reading the file/env) happens in the daemon
/// at share time, not in this crate — this type only carries the declaration.
///
/// Adding a variant here means updating, in lockstep: `Serialize` /
/// `secret_source_from_value` below (deserialize is a catch-all `Err`, so a new
/// variant compiles + serializes but *silently fails to parse* until added),
/// the `Debug` redaction below, `resolve_secret` in the daemon
/// (`share/endpoint.rs`), and the `SecretSource` `$def` in
/// `schema/v2/veld.schema.json` (hand-maintained — no compiler check ties it to
/// this enum).
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum SecretSource {
    /// The literal secret value, inline in config.
    Literal(String),
    /// Name of an environment variable holding the secret.
    Env(String),
    /// Path to a file whose (trimmed) contents are the secret.
    File(String),
    /// A shell command whose (trimmed) stdout is the secret.
    Command(String),
}

impl std::fmt::Debug for SecretSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render a literal secret — it could otherwise leak into logs /
        // error output. The reference forms (env name, file path, command) are
        // not themselves secret and stay visible for debugging.
        match self {
            SecretSource::Literal(_) => f.write_str("Literal(\"***\")"),
            SecretSource::Env(v) => f.debug_tuple("Env").field(v).finish(),
            SecretSource::File(p) => f.debug_tuple("File").field(p).finish(),
            SecretSource::Command(c) => f.debug_tuple("Command").field(c).finish(),
        }
    }
}

impl Serialize for SecretSource {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        match self {
            SecretSource::Literal(v) => s.serialize_str(v),
            SecretSource::Env(v) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("env", v)?;
                m.end()
            }
            SecretSource::File(v) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("file", v)?;
                m.end()
            }
            SecretSource::Command(v) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("command", v)?;
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for SecretSource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        secret_source_from_value(serde_json::Value::deserialize(d)?).map_err(D::Error::custom)
    }
}

/// Parse a [`SecretSource`] from a JSON value: a string is a literal secret; an
/// object must carry exactly one of `env` / `file` / `command` with a string
/// value.
fn secret_source_from_value(value: serde_json::Value) -> Result<SecretSource, String> {
    match value {
        serde_json::Value::String(s) => Ok(SecretSource::Literal(s)),
        serde_json::Value::Object(map) => {
            if map.len() != 1 {
                return Err(
                    "token object must have exactly one of \"env\", \"file\", or \"command\""
                        .to_owned(),
                );
            }
            let (key, val) = map.into_iter().next().expect("len checked == 1");
            let s = val
                .as_str()
                .ok_or_else(|| format!("token \"{key}\" must be a string"))?
                .to_owned();
            match key.as_str() {
                "env" => Ok(SecretSource::Env(s)),
                "file" => Ok(SecretSource::File(s)),
                "command" => Ok(SecretSource::Command(s)),
                other => Err(format!(
                    "unknown token source \"{other}\"; expected \"env\", \"file\", or \"command\""
                )),
            }
        }
        _ => Err("token must be a string or an { env | file | command } object".to_owned()),
    }
}

impl Serialize for RelayEntry {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        match &self.token {
            // No token → bare string, so token-less configs round-trip to the
            // original list-of-URLs form.
            None => s.serialize_str(&self.url),
            Some(token) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("url", &self.url)?;
                m.serialize_entry("token", token)?;
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for RelayEntry {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        relay_entry_from_value(serde_json::Value::deserialize(d)?).map_err(D::Error::custom)
    }
}

/// Parse a [`RelayEntry`] from a JSON value: a bare string is a token-less URL;
/// an object must carry a `url` string and may carry a `token`.
fn relay_entry_from_value(value: serde_json::Value) -> Result<RelayEntry, String> {
    match value {
        serde_json::Value::String(url) => Ok(RelayEntry { url, token: None }),
        serde_json::Value::Object(mut map) => {
            let url = map
                .remove("url")
                .ok_or("relay entry object must have a \"url\"")?;
            let url = url
                .as_str()
                .ok_or("relay entry \"url\" must be a string")?
                .to_owned();
            let token = match map.remove("token") {
                Some(t) => Some(secret_source_from_value(t)?),
                None => None,
            };
            if !map.is_empty() {
                let unknown: Vec<&str> = map.keys().map(String::as_str).collect();
                return Err(format!(
                    "unknown key(s) in relay entry: {}; expected \"url\" and optional \"token\"",
                    unknown.join(", ")
                ));
            }
            Ok(RelayEntry { url, token })
        }
        _ => Err("relay entry must be a URL string or a { url, token } object".to_owned()),
    }
}

impl Serialize for RelayPolicy {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            RelayPolicy::Public => s.serialize_str("public"),
            RelayPolicy::Custom(entries) => entries.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for RelayPolicy {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::String(s) if s == "public" => Ok(RelayPolicy::Public),
            serde_json::Value::String(s) => Err(D::Error::custom(format!(
                "relays must be \"public\" or an array of relay URLs (or {{ url, token }} \
                 objects), got \"{s}\""
            ))),
            serde_json::Value::Array(arr) => {
                if arr.is_empty() {
                    return Err(D::Error::custom(
                        "relays array must not be empty; use \"public\" for public relays",
                    ));
                }
                let entries: Vec<RelayEntry> = arr
                    .into_iter()
                    .map(relay_entry_from_value)
                    .collect::<Result<_, _>>()
                    .map_err(D::Error::custom)?;
                Ok(RelayPolicy::Custom(entries))
            }
            _ => Err(D::Error::custom(
                "relays must be \"public\" or an array of relay URLs (or { url, token } objects)",
            )),
        }
    }
}

/// Per-variant sharing opt-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharePolicy {
    /// Audiences this service may be exposed to. Empty means not shareable.
    #[serde(default)]
    pub expose: Vec<ExposeMode>,
}

impl SharePolicy {
    /// Whether this policy permits the given audience.
    pub fn allows(&self, mode: ExposeMode) -> bool {
        self.expose.contains(&mode)
    }
}

/// A sharing audience.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExposeMode {
    /// Other Veld users. The origin URL is reproduced verbatim on the consumer.
    Peer,
    /// Any browser, via the public web gateway. Best-effort URL fidelity.
    Web,
}

// ---------------------------------------------------------------------------
// Node / Variant
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_variant: Option<String>,

    /// Optional URL template override for all variants of this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_template: Option<String>,

    /// When true, this node is hidden from `veld nodes` output.
    /// Hidden nodes still participate in the dependency graph normally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,

    /// Client-side log levels override for all variants of this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_log_levels: Option<Vec<String>>,

    /// Feature toggles override for all variants of this node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesConfig>,

    /// Extra environment variables inherited by all variants of this node.
    /// Overrides project-level env. Overridable at variant level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,

    /// Working directory for all variants of this node. Relative paths are resolved from the project root (the directory containing veld.json).
    /// Overridable at variant level. Supports variable substitution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// User-invokable actions for this node, exposed on the CLI (`veld action
    /// <name>`) and as buttons in the management dashboard. Each action runs a
    /// shell command with the node's live outputs available as variables and
    /// environment variables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<ActionConfig>>,

    pub variants: HashMap<String, VariantConfig>,
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// A user-invokable action attached to a node. Actions generalise the
/// hard-coded "open in Postico" behaviour: any node can declare commands that
/// the CLI and dashboard expose generically (e.g. open a DB client, tail a
/// queue, run a one-off script). The command runs in a shell with the node's
/// live outputs injected both as `${output.KEY}` template variables and as
/// environment variables, plus the action's static `parameters` as
/// `${param.KEY}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionConfig {
    /// Stable identifier used to invoke the action: `veld action <name>`.
    pub name: String,

    /// Human-readable label for the dashboard button. Defaults to `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Optional one-line description shown in `veld actions` and as a tooltip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Shell command to execute. Supports `${veld.*}`, `${output.KEY}` (this
    /// node's live outputs), `${param.KEY}` (this action's parameters), and
    /// `${nodes.name.field}` substitution. The same values are also exported as
    /// environment variables (`$KEY` for outputs, `$KEY` for parameters), so
    /// shell-style references work too.
    pub command: String,

    /// Static key/value parameters baked into the action. Available to the
    /// command as `${param.KEY}` and as `$KEY` environment variables. Values
    /// support `${veld.*}` and `${output.KEY}` substitution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<HashMap<String, String>>,

    /// Output keys that must all be present on the running node for this action
    /// to be available. Gates both CLI invocation and dashboard button
    /// visibility. When omitted, the action is always available for a running
    /// node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_outputs: Option<Vec<String>>,
}

impl ActionConfig {
    /// The label to show in UIs, falling back to the action `name`.
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }

    /// True if `outputs` contains every key listed in `requires_outputs`.
    /// Actions without `requires_outputs` are always considered satisfied.
    pub fn outputs_satisfied(&self, outputs: &HashMap<String, String>) -> bool {
        match &self.requires_outputs {
            Some(keys) => keys.iter().all(|k| outputs.contains_key(k)),
            None => true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantConfig {
    /// Step type: `command` or `start_server`.
    #[serde(rename = "type")]
    pub step_type: StepType,

    /// Inline command string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Path to script file (relative to veld.json).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,

    /// Legacy health check configuration (start_server only).
    /// Deprecated: use `probes.readiness` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheck>,

    /// Readiness and liveness probe configuration.
    /// `probes.readiness` supersedes the legacy `health_check` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probes: Option<ProbesConfig>,

    /// Dependencies: node name -> variant name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<HashMap<String, String>>,

    /// Extra environment variables injected into the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,

    /// Outputs declaration.
    ///
    /// - For `command`: a list of declared output names (`Vec<String>`).
    /// - For `start_server`: a map of synthetic outputs (`HashMap<String, String>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Outputs>,

    /// Output keys whose values are sensitive (masked, encrypted at rest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive_outputs: Option<Vec<String>>,

    /// When true (default), fail if a command produces outputs not declared in `outputs`.
    /// Set to `false` to allow undeclared outputs to pass through.
    #[serde(default = "default_strict_outputs")]
    pub strict_outputs: bool,

    /// Idempotency check — skip this command step if this command exits 0.
    /// Previously named `verify` (still accepted for backward compatibility).
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "verify")]
    pub skip_if: Option<String>,

    /// Optional URL template override for this specific variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_template: Option<String>,

    /// Teardown command to run when the environment is stopped.
    /// Executed in reverse dependency order during `veld stop`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_stop: Option<String>,

    /// Client-side log levels override for this specific variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_log_levels: Option<Vec<String>>,

    /// Feature toggles override for this specific variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesConfig>,

    /// Working directory for this variant. Relative paths are resolved from the project root (the directory containing veld.json).
    /// Overrides node-level `cwd`. Supports variable substitution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Sharing opt-in for this variant. Absent (or an empty `expose` list) means
    /// this service can never be shared — `veld share` refuses it. This is the
    /// explicit, per-service consent that makes sharing auditable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<SharePolicy>,
}

// ---------------------------------------------------------------------------
// Outputs — handles both Vec<String> and HashMap<String,String>
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Outputs {
    /// Declared output names for `command` steps (captured from `$VELD_OUTPUT_FILE` or legacy `VELD_OUTPUT` stdout).
    Declared(Vec<String>),
    /// Synthetic output templates for `start_server` steps.
    Synthetic(HashMap<String, String>),
}

impl Outputs {
    /// Return the set of declared output key names.
    pub fn declared_keys(&self) -> HashSet<&str> {
        match self {
            Outputs::Declared(keys) => keys.iter().map(|s| s.as_str()).collect(),
            Outputs::Synthetic(map) => map.keys().map(|s| s.as_str()).collect(),
        }
    }
}

impl<'de> Deserialize<'de> for Outputs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Array(arr) => {
                let items: Vec<String> = arr
                    .into_iter()
                    .map(|v| {
                        v.as_str().map(|s| s.to_owned()).ok_or_else(|| {
                            serde::de::Error::custom("outputs array must contain strings")
                        })
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Outputs::Declared(items))
            }
            serde_json::Value::Object(map) => {
                let items: HashMap<String, String> = map
                    .into_iter()
                    .map(|(k, v)| {
                        let s = v.as_str().map(|s| s.to_owned()).ok_or_else(|| {
                            serde::de::Error::custom("outputs map values must be strings")
                        })?;
                        Ok((k, s))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Outputs::Synthetic(items))
            }
            _ => Err(serde::de::Error::custom(
                "outputs must be an array of strings or an object of string values",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Features
// ---------------------------------------------------------------------------

/// Per-node feature toggles. All fields are optional — `None` means "inherit
/// from the parent level". The resolution order is variant > node > project,
/// with the built-in defaults as final fallback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturesConfig {
    /// Inject the feedback overlay toolbar into HTML responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback_overlay: Option<bool>,

    /// Inject the client-side log collector into HTML responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_logs: Option<bool>,

    /// Automatically inject bootstrap scripts into HTML responses. When `false`,
    /// the `/__veld__/*` proxy routes are still created so you can manually add
    /// `<script src="/__veld__/...">` tags in your app. Default: `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject: Option<bool>,
}

/// Resolved (concrete) feature flags — no more `Option`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedFeatures {
    pub feedback_overlay: bool,
    pub client_logs: bool,
    pub inject: bool,
}

impl Default for ResolvedFeatures {
    fn default() -> Self {
        Self {
            feedback_overlay: true,
            client_logs: true,
            inject: true,
        }
    }
}

/// Resolve feature flags using the most specific override:
/// variant > node > project > default (`true`).
pub fn resolve_features(
    project: Option<&FeaturesConfig>,
    node: Option<&FeaturesConfig>,
    variant: Option<&FeaturesConfig>,
) -> ResolvedFeatures {
    let layers: &[Option<&FeaturesConfig>] = &[variant, node, project];
    let defaults = ResolvedFeatures::default();

    ResolvedFeatures {
        feedback_overlay: layers
            .iter()
            .filter_map(|l| l.and_then(|f| f.feedback_overlay))
            .next()
            .unwrap_or(defaults.feedback_overlay),
        client_logs: layers
            .iter()
            .filter_map(|l| l.and_then(|f| f.client_logs))
            .next()
            .unwrap_or(defaults.client_logs),
        inject: layers
            .iter()
            .filter_map(|l| l.and_then(|f| f.inject))
            .next()
            .unwrap_or(defaults.inject),
    }
}

/// Merge environment variable maps using the most specific override:
/// variant > node > project. For each key, the most specific layer wins.
pub fn resolve_env(
    project: Option<&HashMap<String, String>>,
    node: Option<&HashMap<String, String>>,
    variant: Option<&HashMap<String, String>>,
) -> Option<HashMap<String, String>> {
    let layers: &[Option<&HashMap<String, String>>] = &[project, node, variant];
    let has_any = layers.iter().any(|l| l.is_some());
    if !has_any {
        return None;
    }

    let mut merged = HashMap::new();
    // Apply from least specific to most specific so later layers override.
    for map in layers.iter().flatten() {
        for (k, v) in *map {
            merged.insert(k.clone(), v.clone());
        }
    }
    Some(merged)
}

// ---------------------------------------------------------------------------
// StepType enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepType {
    #[serde(rename = "command", alias = "bash")]
    Command,
    #[serde(rename = "start_server")]
    StartServer,
}

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCheck {
    /// One of "http", "port", "command".
    #[serde(rename = "type")]
    pub check_type: String,

    /// HTTP path for type "http".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Expected HTTP status code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_status: Option<u16>,

    /// Command for type "command".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Maximum seconds to wait for health (default 60).
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,

    /// Milliseconds between checks (default 1000).
    #[serde(default = "default_interval")]
    pub interval_ms: u64,
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Probes
// ---------------------------------------------------------------------------

/// Readiness and liveness probe configuration for a variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbesConfig {
    /// Readiness probe — gates the dependency graph during startup.
    /// Same semantics as the legacy `health_check` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<HealthCheck>,

    /// Liveness probe — runs continuously after the node is healthy.
    /// Triggers recovery when `failure_threshold` consecutive checks fail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub liveness: Option<LivenessProbe>,
}

/// Liveness probe configuration. Shares check-type fields with `HealthCheck`
/// but adds failure thresholds and recovery limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivenessProbe {
    /// One of "http", "port", "command".
    #[serde(rename = "type")]
    pub check_type: String,

    /// HTTP path for type "http".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// Expected HTTP status code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_status: Option<u16>,

    /// Command for type "command".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Milliseconds between liveness checks (default 5000).
    #[serde(default = "default_liveness_interval")]
    pub interval_ms: u64,

    /// Consecutive failures before triggering recovery (default 3).
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,

    /// Maximum number of recovery attempts before permanent failure (default 3).
    #[serde(default = "default_max_recoveries")]
    pub max_recoveries: u32,
}

fn default_liveness_interval() -> u64 {
    5000
}

fn default_failure_threshold() -> u32 {
    3
}

fn default_max_recoveries() -> u32 {
    3
}

impl VariantConfig {
    /// Resolve the effective readiness probe: `probes.readiness` takes
    /// precedence over the legacy `health_check` field.
    pub fn readiness_probe(&self) -> Option<&HealthCheck> {
        self.probes
            .as_ref()
            .and_then(|p| p.readiness.as_ref())
            .or(self.health_check.as_ref())
    }

    /// Return the liveness probe if configured.
    pub fn liveness_probe(&self) -> Option<&LivenessProbe> {
        self.probes.as_ref().and_then(|p| p.liveness.as_ref())
    }
}

fn default_strict_outputs() -> bool {
    true
}

fn default_timeout() -> u64 {
    60
}

fn default_interval() -> u64 {
    1000
}

// ---------------------------------------------------------------------------
// Config discovery + loading
// ---------------------------------------------------------------------------

/// Walk upward from `start` to find `veld.json`. Returns the path to the file.
pub fn discover_config(start: &Path) -> Result<PathBuf, ConfigError> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("veld.json");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !dir.pop() {
            return Err(ConfigError::NotFound(start.to_path_buf()));
        }
    }
}

/// Load and parse the config from a discovered path.
pub fn load_config(path: &Path) -> Result<VeldConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;

    let config: VeldConfig =
        serde_json::from_str(&contents).map_err(|e| ConfigError::ParseError {
            path: path.to_path_buf(),
            source: e,
        })?;

    if config.schema_version != "1" && config.schema_version != "2" {
        return Err(ConfigError::UnsupportedSchemaVersion(
            config.schema_version.clone(),
        ));
    }

    Ok(config)
}

/// Convenience: discover from CWD and load.
pub fn load_config_from_cwd() -> Result<(PathBuf, VeldConfig), ConfigError> {
    let cwd = std::env::current_dir().map_err(|e| ConfigError::ReadError {
        path: PathBuf::from("."),
        source: e,
    })?;
    let path = discover_config(&cwd)?;
    let config = load_config(&path)?;
    Ok((path, config))
}

/// Default client log levels when none are configured.
pub const DEFAULT_CLIENT_LOG_LEVELS: &[&str] = &["log", "warn", "error"];

/// Valid client log level values.
const VALID_CLIENT_LOG_LEVELS: &[&str] = &["log", "warn", "error", "info", "debug"];

/// Resolve the effective client log levels for a given node+variant,
/// using the most specific override: variant > node > project > default.
/// Invalid level values are silently filtered out.
pub fn resolve_client_log_levels(
    project_levels: Option<&[String]>,
    node_levels: Option<&[String]>,
    variant_levels: Option<&[String]>,
) -> Vec<String> {
    let raw = if let Some(levels) = variant_levels {
        levels.to_vec()
    } else if let Some(levels) = node_levels {
        levels.to_vec()
    } else if let Some(levels) = project_levels {
        levels.to_vec()
    } else {
        return DEFAULT_CLIENT_LOG_LEVELS
            .iter()
            .map(|s| s.to_string())
            .collect();
    };
    // Filter to only valid values. If nothing remains, fall back to defaults.
    let filtered: Vec<String> = raw
        .into_iter()
        .filter(|l| VALID_CLIENT_LOG_LEVELS.contains(&l.as_str()))
        .collect();
    if filtered.is_empty() {
        DEFAULT_CLIENT_LOG_LEVELS
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        filtered
    }
}

/// Resolve the effective working directory for a node+variant.
/// Uses the most specific override: variant > node > project root.
/// Relative paths are resolved against the project root.
pub fn resolve_cwd(
    project_root: &Path,
    node_cwd: Option<&str>,
    variant_cwd: Option<&str>,
) -> PathBuf {
    let raw = variant_cwd.or(node_cwd);
    match raw {
        Some(dir) => {
            let p = Path::new(dir);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                project_root.join(p)
            }
        }
        None => project_root.to_path_buf(),
    }
}

/// Return the project root directory (parent of veld.json).
pub fn project_root(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .expect("veld.json must have a parent directory")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_client_log_levels_defaults() {
        let result = resolve_client_log_levels(None, None, None);
        assert_eq!(result, vec!["log", "warn", "error"]);
    }

    #[test]
    fn test_resolve_client_log_levels_project_override() {
        let project = vec!["warn".to_string(), "error".to_string()];
        let result = resolve_client_log_levels(Some(&project), None, None);
        assert_eq!(result, vec!["warn", "error"]);
    }

    #[test]
    fn test_resolve_client_log_levels_node_overrides_project() {
        let project = vec!["warn".to_string()];
        let node = vec!["log".to_string(), "info".to_string()];
        let result = resolve_client_log_levels(Some(&project), Some(&node), None);
        assert_eq!(result, vec!["log", "info"]);
    }

    #[test]
    fn test_resolve_client_log_levels_variant_overrides_all() {
        let project = vec!["warn".to_string()];
        let node = vec!["log".to_string()];
        let variant = vec!["debug".to_string()];
        let result = resolve_client_log_levels(Some(&project), Some(&node), Some(&variant));
        assert_eq!(result, vec!["debug"]);
    }

    #[test]
    fn test_resolve_client_log_levels_filters_invalid() {
        let project = vec!["log".to_string(), "bogus".to_string(), "error".to_string()];
        let result = resolve_client_log_levels(Some(&project), None, None);
        assert_eq!(result, vec!["log", "error"]);
    }

    #[test]
    fn test_resolve_client_log_levels_all_invalid_falls_back_to_default() {
        let project = vec!["bogus".to_string(), "invalid".to_string()];
        let result = resolve_client_log_levels(Some(&project), None, None);
        assert_eq!(result, vec!["log", "warn", "error"]);
    }

    // -- Features resolution tests --------------------------------------------

    #[test]
    fn test_resolve_features_defaults() {
        let result = resolve_features(None, None, None);
        assert!(result.feedback_overlay);
        assert!(result.client_logs);
        assert!(result.inject);
    }

    #[test]
    fn test_resolve_features_project_override() {
        let project = FeaturesConfig {
            feedback_overlay: Some(false),
            client_logs: None,
            inject: None,
        };
        let result = resolve_features(Some(&project), None, None);
        assert!(!result.feedback_overlay);
        assert!(result.client_logs);
        assert!(result.inject);
    }

    #[test]
    fn test_resolve_features_node_overrides_project() {
        let project = FeaturesConfig {
            feedback_overlay: Some(false),
            client_logs: Some(false),
            inject: None,
        };
        let node = FeaturesConfig {
            feedback_overlay: Some(true),
            client_logs: None,
            inject: None,
        };
        let result = resolve_features(Some(&project), Some(&node), None);
        assert!(result.feedback_overlay); // node wins
        assert!(!result.client_logs); // falls through to project
    }

    #[test]
    fn test_resolve_features_variant_overrides_all() {
        let project = FeaturesConfig {
            feedback_overlay: Some(true),
            client_logs: Some(true),
            inject: Some(true),
        };
        let node = FeaturesConfig {
            feedback_overlay: Some(true),
            client_logs: Some(true),
            inject: Some(true),
        };
        let variant = FeaturesConfig {
            feedback_overlay: Some(false),
            client_logs: Some(false),
            inject: Some(false),
        };
        let result = resolve_features(Some(&project), Some(&node), Some(&variant));
        assert!(!result.feedback_overlay);
        assert!(!result.client_logs);
        assert!(!result.inject);
    }

    #[test]
    fn test_resolve_features_inject_false_keeps_features() {
        let project = FeaturesConfig {
            feedback_overlay: None,
            client_logs: None,
            inject: Some(false),
        };
        let result = resolve_features(Some(&project), None, None);
        assert!(result.feedback_overlay); // still true
        assert!(result.client_logs); // still true
        assert!(!result.inject); // injection disabled
    }

    #[test]
    fn test_resolve_features_inject_variant_overrides_project() {
        let project = FeaturesConfig {
            feedback_overlay: None,
            client_logs: None,
            inject: Some(false),
        };
        let variant = FeaturesConfig {
            feedback_overlay: None,
            client_logs: None,
            inject: Some(true),
        };
        let result = resolve_features(Some(&project), None, Some(&variant));
        assert!(result.inject); // variant wins
    }

    // -- cwd resolution tests -------------------------------------------------

    #[test]
    fn test_resolve_cwd_defaults_to_project_root() {
        let root = PathBuf::from("/projects/myapp");
        let result = resolve_cwd(&root, None, None);
        assert_eq!(result, PathBuf::from("/projects/myapp"));
    }

    #[test]
    fn test_resolve_cwd_node_level() {
        let root = PathBuf::from("/projects/myapp");
        let result = resolve_cwd(&root, Some("packages/api"), None);
        assert_eq!(result, PathBuf::from("/projects/myapp/packages/api"));
    }

    #[test]
    fn test_resolve_cwd_variant_overrides_node() {
        let root = PathBuf::from("/projects/myapp");
        let result = resolve_cwd(&root, Some("packages/api"), Some("packages/frontend"));
        assert_eq!(result, PathBuf::from("/projects/myapp/packages/frontend"));
    }

    #[test]
    fn test_resolve_cwd_absolute_path() {
        let root = PathBuf::from("/projects/myapp");
        let result = resolve_cwd(&root, None, Some("/opt/services/api"));
        assert_eq!(result, PathBuf::from("/opt/services/api"));
    }

    // -- Env resolution tests --------------------------------------------------

    #[test]
    fn test_resolve_env_none() {
        assert_eq!(resolve_env(None, None, None), None);
    }

    #[test]
    fn test_resolve_env_project_only() {
        let project = HashMap::from([("A".into(), "1".into())]);
        let result = resolve_env(Some(&project), None, None).unwrap();
        assert_eq!(result.get("A").unwrap(), "1");
    }

    #[test]
    fn test_resolve_env_node_overrides_project() {
        let project = HashMap::from([("A".into(), "1".into()), ("B".into(), "2".into())]);
        let node = HashMap::from([("A".into(), "override".into())]);
        let result = resolve_env(Some(&project), Some(&node), None).unwrap();
        assert_eq!(result.get("A").unwrap(), "override");
        assert_eq!(result.get("B").unwrap(), "2");
    }

    #[test]
    fn test_resolve_env_variant_overrides_all() {
        let project = HashMap::from([("A".into(), "1".into())]);
        let node = HashMap::from([("A".into(), "2".into()), ("B".into(), "3".into())]);
        let variant = HashMap::from([("A".into(), "final".into()), ("C".into(), "4".into())]);
        let result = resolve_env(Some(&project), Some(&node), Some(&variant)).unwrap();
        assert_eq!(result.get("A").unwrap(), "final");
        assert_eq!(result.get("B").unwrap(), "3");
        assert_eq!(result.get("C").unwrap(), "4");
    }

    #[test]
    fn test_resolve_env_empty_map_with_values() {
        let empty = HashMap::new();
        let variant = HashMap::from([("X".into(), "1".into())]);
        let result = resolve_env(Some(&empty), None, Some(&variant)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("X").unwrap(), "1");
    }

    #[test]
    fn test_resolve_env_all_empty_maps() {
        let empty = HashMap::new();
        let result = resolve_env(Some(&empty), Some(&empty), Some(&empty)).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_env_variant_only() {
        let variant = HashMap::from([("X".into(), "val".into())]);
        let result = resolve_env(None, None, Some(&variant)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("X").unwrap(), "val");
    }

    #[test]
    fn test_resolve_cwd_variant_none_falls_through_to_node() {
        let root = PathBuf::from("/projects/myapp");
        let result = resolve_cwd(&root, Some("subdir"), None);
        assert_eq!(result, PathBuf::from("/projects/myapp/subdir"));
    }

    // -- Setup / Teardown deserialization tests --------------------------------

    #[test]
    fn test_setup_step_deserialization() {
        let json = r#"{"name": "docker", "command": "docker info", "failureMessage": "Docker must be running"}"#;
        let step: SetupStep = serde_json::from_str(json).unwrap();
        assert_eq!(step.name, "docker");
        assert_eq!(step.command, "docker info");
        assert_eq!(
            step.failure_message.as_deref(),
            Some("Docker must be running")
        );
    }

    #[test]
    fn test_setup_step_without_failure_message() {
        let json = r#"{"name": "network", "command": "docker network create veld"}"#;
        let step: SetupStep = serde_json::from_str(json).unwrap();
        assert_eq!(step.name, "network");
        assert_eq!(step.command, "docker network create veld");
        assert!(step.failure_message.is_none());
    }

    #[test]
    fn test_config_with_setup_and_teardown() {
        let json = r#"{
            "schemaVersion": "1",
            "name": "test-project",
            "setup": [
                {"name": "check", "command": "echo ok", "failureMessage": "Check failed"},
                {"name": "init", "command": "mkdir -p /tmp/test"}
            ],
            "teardown": [
                {"name": "cleanup", "command": "rm -rf /tmp/test"}
            ],
            "nodes": {
                "app": {
                    "variants": {
                        "local": {
                            "type": "start_server",
                            "command": "echo start"
                        }
                    }
                }
            }
        }"#;
        let config: VeldConfig = serde_json::from_str(json).unwrap();
        let setup = config.setup.as_ref().unwrap();
        assert_eq!(setup.len(), 2);
        assert_eq!(setup[0].name, "check");
        assert_eq!(setup[0].failure_message.as_deref(), Some("Check failed"));
        assert_eq!(setup[1].name, "init");
        assert!(setup[1].failure_message.is_none());

        let teardown = config.teardown.as_ref().unwrap();
        assert_eq!(teardown.len(), 1);
        assert_eq!(teardown[0].name, "cleanup");
    }

    #[test]
    fn test_config_without_setup_teardown() {
        let json = r#"{
            "schemaVersion": "1",
            "name": "test-project",
            "nodes": {
                "app": {
                    "variants": {
                        "local": {
                            "type": "start_server",
                            "command": "echo start"
                        }
                    }
                }
            }
        }"#;
        let config: VeldConfig = serde_json::from_str(json).unwrap();
        assert!(config.setup.is_none());
        assert!(config.teardown.is_none());
    }

    // -- Probes config tests ---------------------------------------------------

    #[test]
    fn test_probes_config_deserialization() {
        let json = r#"{
            "readiness": {
                "type": "http",
                "path": "/health",
                "timeout_seconds": 30,
                "interval_ms": 500
            },
            "liveness": {
                "type": "command",
                "command": "pg_isready",
                "interval_ms": 5000,
                "failure_threshold": 5,
                "max_recoveries": 2
            }
        }"#;
        let probes: ProbesConfig = serde_json::from_str(json).unwrap();
        let readiness = probes.readiness.unwrap();
        assert_eq!(readiness.check_type, "http");
        assert_eq!(readiness.path.as_deref(), Some("/health"));
        assert_eq!(readiness.timeout_seconds, 30);

        let liveness = probes.liveness.unwrap();
        assert_eq!(liveness.check_type, "command");
        assert_eq!(liveness.command.as_deref(), Some("pg_isready"));
        assert_eq!(liveness.interval_ms, 5000);
        assert_eq!(liveness.failure_threshold, 5);
        assert_eq!(liveness.max_recoveries, 2);
    }

    #[test]
    fn test_liveness_probe_defaults() {
        let json = r#"{"type": "command", "command": "true"}"#;
        let liveness: LivenessProbe = serde_json::from_str(json).unwrap();
        assert_eq!(liveness.interval_ms, 5000);
        assert_eq!(liveness.failure_threshold, 3);
        assert_eq!(liveness.max_recoveries, 3);
    }

    // -- skip_if / verify alias tests ------------------------------------------

    #[test]
    fn test_skip_if_field() {
        let json = r#"{
            "type": "command",
            "command": "echo run",
            "skip_if": "test -f /tmp/done"
        }"#;
        let v: VariantConfig = serde_json::from_str(json).unwrap();
        assert_eq!(v.skip_if.as_deref(), Some("test -f /tmp/done"));
    }

    #[test]
    fn test_verify_alias_for_skip_if() {
        let json = r#"{
            "type": "command",
            "command": "echo run",
            "verify": "test -f /tmp/done"
        }"#;
        let v: VariantConfig = serde_json::from_str(json).unwrap();
        assert_eq!(v.skip_if.as_deref(), Some("test -f /tmp/done"));
    }

    // -- Schema version tests --------------------------------------------------

    #[test]
    fn test_schema_version_2_accepted() {
        let json = r#"{
            "schemaVersion": "2",
            "name": "test-project",
            "nodes": {
                "db": {
                    "variants": {
                        "local": {
                            "type": "command",
                            "command": "echo start",
                            "probes": {
                                "liveness": {
                                    "type": "command",
                                    "command": "pg_isready"
                                }
                            }
                        }
                    }
                }
            }
        }"#;
        let config: VeldConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.schema_version, "2");
        let variant = &config.nodes["db"].variants["local"];
        assert!(variant.probes.is_some());
        let liveness = variant.liveness_probe().unwrap();
        assert_eq!(liveness.check_type, "command");
    }

    // -- Action config tests ---------------------------------------------------

    #[test]
    fn test_action_minimal_deserialization() {
        let json = r#"{"name": "psql", "command": "psql $DB_URL"}"#;
        let action: ActionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(action.name, "psql");
        assert_eq!(action.command, "psql $DB_URL");
        // label falls back to name; no params or gating by default.
        assert_eq!(action.display_label(), "psql");
        assert!(action.parameters.is_none());
        assert!(action.outputs_satisfied(&HashMap::new()));
    }

    #[test]
    fn test_action_full_deserialization() {
        let json = r#"{
            "name": "postico",
            "label": "Postico",
            "description": "Open the database in Postico",
            "command": "open -a Postico \"postgresql://${output.DB_USER}@${output.DB_HOST}:${output.DB_PORT}/${output.DB_NAME}\"",
            "parameters": {"app": "Postico"},
            "requires_outputs": ["DB_HOST", "DB_PORT", "DB_NAME"]
        }"#;
        let action: ActionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(action.name, "postico");
        assert_eq!(action.display_label(), "Postico");
        assert_eq!(
            action.description.as_deref(),
            Some("Open the database in Postico")
        );
        assert_eq!(
            action.parameters.as_ref().unwrap().get("app").unwrap(),
            "Postico"
        );

        let mut outputs = HashMap::new();
        outputs.insert("DB_HOST".to_string(), "localhost".to_string());
        assert!(!action.outputs_satisfied(&outputs)); // missing DB_PORT, DB_NAME
        outputs.insert("DB_PORT".to_string(), "5432".to_string());
        outputs.insert("DB_NAME".to_string(), "app".to_string());
        assert!(action.outputs_satisfied(&outputs));
    }

    #[test]
    fn test_node_config_with_actions() {
        let json = r#"{
            "variants": {"dblab": {"type": "start_server", "command": "ssh -L ..."}},
            "actions": [
                {"name": "postico", "command": "open -a Postico", "requires_outputs": ["DB_HOST"]}
            ]
        }"#;
        let node: NodeConfig = serde_json::from_str(json).unwrap();
        let actions = node.actions.unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name, "postico");
    }

    // -- Readiness probe helper tests ------------------------------------------

    #[test]
    fn test_readiness_probe_from_probes() {
        let json = r#"{
            "type": "start_server",
            "command": "npm start",
            "probes": {
                "readiness": {
                    "type": "http",
                    "path": "/health"
                }
            }
        }"#;
        let v: VariantConfig = serde_json::from_str(json).unwrap();
        let probe = v.readiness_probe().unwrap();
        assert_eq!(probe.check_type, "http");
    }

    #[test]
    fn test_readiness_probe_fallback_to_health_check() {
        let json = r#"{
            "type": "start_server",
            "command": "npm start",
            "health_check": {
                "type": "port"
            }
        }"#;
        let v: VariantConfig = serde_json::from_str(json).unwrap();
        let probe = v.readiness_probe().unwrap();
        assert_eq!(probe.check_type, "port");
    }

    #[test]
    fn test_readiness_probe_probes_overrides_health_check() {
        let json = r#"{
            "type": "start_server",
            "command": "npm start",
            "health_check": {
                "type": "port"
            },
            "probes": {
                "readiness": {
                    "type": "http",
                    "path": "/ready"
                }
            }
        }"#;
        let v: VariantConfig = serde_json::from_str(json).unwrap();
        let probe = v.readiness_probe().unwrap();
        assert_eq!(probe.check_type, "http");
    }

    // -- Sharing config tests -------------------------------------------------

    #[test]
    fn test_relay_policy_public_string() {
        let p: RelayPolicy = serde_json::from_str(r#""public""#).unwrap();
        assert_eq!(p, RelayPolicy::Public);
        // round-trips back to the string form
        assert_eq!(serde_json::to_string(&p).unwrap(), r#""public""#);
    }

    #[test]
    fn test_relay_policy_custom_list() {
        let p: RelayPolicy = serde_json::from_str(r#"["https://relay.example.com"]"#).unwrap();
        assert_eq!(
            p,
            RelayPolicy::Custom(vec![RelayEntry::url("https://relay.example.com")])
        );
        // A token-less entry round-trips back to the bare-string list form.
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"["https://relay.example.com"]"#
        );
    }

    #[test]
    fn test_relay_policy_rejects_empty_list() {
        assert!(serde_json::from_str::<RelayPolicy>("[]").is_err());
    }

    #[test]
    fn test_sharing_embed_flag_defaults_false_and_uses_camelcase_key() {
        // Absent → false.
        let s: SharingConfig = serde_json::from_str(r#"{"relays":"public"}"#).unwrap();
        assert!(!s.dangerously_embed_relay_tokens_in_ticket);
        // Present via the React-style camelCase key → true.
        let s: SharingConfig = serde_json::from_str(
            r#"{"relays":"public","dangerouslyEmbedRelayTokensInTicket":true}"#,
        )
        .unwrap();
        assert!(s.dangerously_embed_relay_tokens_in_ticket);
        // Serializes back with the camelCase key when true.
        assert!(
            serde_json::to_string(&s)
                .unwrap()
                .contains("dangerouslyEmbedRelayTokensInTicket")
        );
        // Omitted entirely when false (no noise in ordinary configs).
        let off = SharingConfig {
            relays: Some(RelayPolicy::Public),
            gateway: None,
            dangerously_embed_relay_tokens_in_ticket: false,
        };
        assert!(
            !serde_json::to_string(&off)
                .unwrap()
                .contains("dangerouslyEmbed")
        );
    }

    #[test]
    fn test_relay_policy_rejects_unknown_string() {
        assert!(serde_json::from_str::<RelayPolicy>(r#""private""#).is_err());
    }

    #[test]
    fn test_relay_policy_mixed_tokens() {
        let json = r#"[
            "https://open.example.com",
            { "url": "https://lit.example.com", "token": "s3cret" },
            { "url": "https://env.example.com", "token": { "env": "RELAY_TOKEN" } },
            { "url": "https://file.example.com", "token": { "file": "/run/secrets/relay" } },
            { "url": "https://cmd.example.com", "token": { "command": "op read op://v/t" } }
        ]"#;
        let p: RelayPolicy = serde_json::from_str(json).unwrap();
        assert_eq!(
            p,
            RelayPolicy::Custom(vec![
                RelayEntry::url("https://open.example.com"),
                RelayEntry {
                    url: "https://lit.example.com".into(),
                    token: Some(SecretSource::Literal("s3cret".into())),
                },
                RelayEntry {
                    url: "https://env.example.com".into(),
                    token: Some(SecretSource::Env("RELAY_TOKEN".into())),
                },
                RelayEntry {
                    url: "https://file.example.com".into(),
                    token: Some(SecretSource::File("/run/secrets/relay".into())),
                },
                RelayEntry {
                    url: "https://cmd.example.com".into(),
                    token: Some(SecretSource::Command("op read op://v/t".into())),
                },
            ])
        );
    }

    #[test]
    fn test_relay_entry_with_token_round_trips() {
        let entry = RelayEntry {
            url: "https://relay.example.com".into(),
            token: Some(SecretSource::Env("RELAY_TOKEN".into())),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            json,
            r#"{"url":"https://relay.example.com","token":{"env":"RELAY_TOKEN"}}"#
        );
        assert_eq!(serde_json::from_str::<RelayEntry>(&json).unwrap(), entry);
    }

    #[test]
    fn test_secret_source_all_variants_round_trip() {
        // Every variant must survive serialize → deserialize. `Serialize` is an
        // exhaustive match (compiler-checked) but deserialize dispatch is a
        // catch-all, so a new variant can silently fail to parse — this guards it.
        for src in [
            SecretSource::Literal("lit".into()),
            SecretSource::Env("VAR".into()),
            SecretSource::File("/run/secrets/relay".into()),
            SecretSource::Command("op read op://v/t".into()),
        ] {
            let json = serde_json::to_string(&src).unwrap();
            assert_eq!(
                serde_json::from_str::<SecretSource>(&json).unwrap(),
                src,
                "round-trip failed for {json}"
            );
        }
    }

    #[test]
    fn test_secret_source_debug_redacts_literal() {
        // A literal secret must never appear in Debug output (logs / errors).
        let dbg = format!("{:?}", SecretSource::Literal("hunter2".into()));
        assert!(!dbg.contains("hunter2"), "literal leaked: {dbg}");
        // Reference forms stay visible — they are not themselves secret.
        assert!(format!("{:?}", SecretSource::Env("VAR".into())).contains("VAR"));
    }

    #[test]
    fn test_relay_entry_rejects_token_object_with_multiple_keys() {
        let json = r#"{ "url": "https://r", "token": { "env": "A", "file": "/b" } }"#;
        assert!(serde_json::from_str::<RelayEntry>(json).is_err());
    }

    #[test]
    fn test_relay_entry_rejects_unknown_token_source() {
        let json = r#"{ "url": "https://r", "token": { "vault": "x" } }"#;
        assert!(serde_json::from_str::<RelayEntry>(json).is_err());
    }

    #[test]
    fn test_relay_entry_rejects_unknown_key() {
        let json = r#"{ "url": "https://r", "auth": "x" }"#;
        assert!(serde_json::from_str::<RelayEntry>(json).is_err());
    }

    #[test]
    fn test_relay_entry_rejects_missing_url() {
        let json = r#"{ "token": "x" }"#;
        assert!(serde_json::from_str::<RelayEntry>(json).is_err());
    }

    #[test]
    fn test_share_policy_allows() {
        let json = r#"{ "expose": ["peer", "web"] }"#;
        let s: SharePolicy = serde_json::from_str(json).unwrap();
        assert!(s.allows(ExposeMode::Peer));
        assert!(s.allows(ExposeMode::Web));
    }

    #[test]
    fn test_share_policy_empty_expose_allows_nothing() {
        let s: SharePolicy = serde_json::from_str(r#"{ "expose": [] }"#).unwrap();
        assert!(!s.allows(ExposeMode::Peer));
        assert!(!s.allows(ExposeMode::Web));
    }

    #[test]
    fn test_variant_share_defaults_to_none() {
        let v: VariantConfig =
            serde_json::from_str(r#"{ "type": "start_server", "command": "x" }"#).unwrap();
        assert!(v.share.is_none());
    }

    #[test]
    fn test_sharing_config_parses_on_veld_config() {
        let json = r#"{
            "schemaVersion": "2",
            "name": "demo",
            "sharing": {
                "relays": ["https://relay.acme.internal"],
                "gateway": "https://share.acme.internal"
            },
            "nodes": {
                "web": {
                    "variants": {
                        "local": {
                            "type": "start_server",
                            "command": "npm start",
                            "share": { "expose": ["peer"] }
                        }
                    }
                }
            }
        }"#;
        let cfg: VeldConfig = serde_json::from_str(json).unwrap();
        let sharing = cfg.sharing.unwrap();
        assert_eq!(
            sharing.relays,
            Some(RelayPolicy::Custom(vec![RelayEntry::url(
                "https://relay.acme.internal"
            )]))
        );
        assert_eq!(
            sharing.gateway.as_deref(),
            Some("https://share.acme.internal")
        );
        let share = cfg.nodes["web"].variants["local"].share.as_ref().unwrap();
        assert!(share.allows(ExposeMode::Peer));
        assert!(!share.allows(ExposeMode::Web));
    }
}
