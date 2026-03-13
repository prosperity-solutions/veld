use serde::{Deserialize, Deserializer, Serialize};
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

    /// The dependency graph nodes.
    pub nodes: HashMap<String, NodeConfig>,
}

fn default_url_template() -> String {
    "{service}.{run}.{project}.localhost".to_owned()
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

    pub variants: HashMap<String, VariantConfig>,
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

    /// Health check configuration (start_server only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheck>,

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

    /// Idempotency verify command (command steps only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,

    /// Optional URL template override for this specific variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_template: Option<String>,

    /// Teardown command to run when the environment is stopped.
    /// Executed in reverse dependency order during `veld stop`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_stop: Option<String>,
}

// ---------------------------------------------------------------------------
// Outputs — handles both Vec<String> and HashMap<String,String>
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Outputs {
    /// Declared output names for `command` steps (captured from VELD_OUTPUT).
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

    if config.schema_version != "1" {
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

/// Return the project root directory (parent of veld.json).
pub fn project_root(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .expect("veld.json must have a parent directory")
        .to_path_buf()
}
