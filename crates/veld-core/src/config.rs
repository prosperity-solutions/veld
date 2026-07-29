use crate::presets::PresetDef;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, HashMap, HashSet};
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

    #[error(
        "schemaVersion \"{0}\" is no longer supported — this veld reads only \"3\".\n  \
         What changes: `command` becomes `argv` (an array, spawned directly) or `shell` (a \
         string, run via sh -c); a bare-string `on_stop`/`skip_if` becomes \
         `{{ \"argv\": … }}`; and a node output referenced as `${{veld.KEY}}` becomes \
         `${{output.KEY}}`.\n  \
         docs/migrating-to-v3.md states every rule — hand it to your coding agent, or \
         apply them yourself. Then run `veld lint` to check the result."
    )]
    UnsupportedSchemaVersion(String),

    #[error("invalid JSONC in {path}: {detail}")]
    Jsonc { path: PathBuf, detail: String },

    #[error(
        "{path} is the root config, so it must declare \"{key}\". Every other key is \
         optional in every file — only the root file needs \"schemaVersion\" and \"name\""
    )]
    MissingRootKey { path: PathBuf, key: &'static str },

    #[error(
        "{path} declares schemaVersion \"3\", where `command` has been replaced by \
         `argv` (an array, spawned directly) or `shell` (a string, run via sh -c). \
         Found the old form at: {}. Use `argv` when the string is a plain program plus \
         arguments, `shell` when it needs a shell (pipes, `&&`, globs, variable \
         expansion). See docs/migrating-to-v3.md.",
        locations.join(", ")
    )]
    LegacyCommandInV3 {
        path: PathBuf,
        locations: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Validation findings
// ---------------------------------------------------------------------------

/// How serious a [`Finding`] is.
///
/// `Error` blocks `veld start`; `Warning` and `Notice` are reported by `veld lint`
/// and never block anything. Nothing here is reachable from [`parse_config`] — see
/// the module note on the parse/validate split.
///
/// The declaration order is load-bearing in exactly one place: [`validate`] sorts
/// findings by it, so errors come first. It deliberately does **not** mean
/// "at least this severe" — `severity >= Severity::Error` would be true for a
/// notice, which is the opposite of what it reads like. Compare with `==`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    /// Nothing is wrong. Used where veld has something to *tell* the author about
    /// a legitimate declaration — currently that `hooks` and `ui` are reserved and
    /// parsed but not executed by this version, which they could not otherwise
    /// discover without reading the changelog.
    Notice,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => f.write_str("error"),
            Severity::Warning => f.write_str("warning"),
            Severity::Notice => f.write_str("notice"),
        }
    }
}

/// One semantic problem found in an already-parsed config.
///
/// `location` is a dotted path into the document (`nodes.api.variants.dev.env`)
/// so the reader can go straight to the line — under `include` globs a bare
/// field name is not enough to find anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub location: String,
    pub message: String,
    /// Which rule produced this, for `veld lint --json` consumers.
    pub rule: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}: {}", self.severity, self.location, self.message)
    }
}

impl Finding {
    fn error(rule: &str, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            location: location.into(),
            message: message.into(),
            rule: rule.to_owned(),
        }
    }

    fn warning(rule: &str, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            location: location.into(),
            message: message.into(),
            rule: rule.to_owned(),
        }
    }

    /// An unrecognised top-level key.
    ///
    /// An error, because the cause is nearly always a typo and silently ignoring
    /// it is how `"noeds"` costs someone an afternoon. But a *reported* error, not
    /// a load failure: see [`crate::include::Document`] for why the loader must
    /// stay lenient about this.
    pub(crate) fn unknown_top_level_key(location: &str, key: &str) -> Self {
        // `"//"` was the way to comment a JSON config before veld accepted real
        // comments, so it is the single likeliest unknown key in an older project.
        // Say what to do instead rather than only refusing.
        let hint = if key == "//" || key.starts_with("//") {
            " Veld now accepts real `//` comments in every config file, so this \
             key can simply become a comment."
                .to_owned()
        } else {
            String::new()
        };
        Self {
            severity: Severity::Error,
            location: location.to_owned(),
            message: format!(
                "unknown top-level key \"{key}\". Expected one of: {}. (`hooks` and `ui` \
                 are reserved and parsed but not executed.){hint}",
                KNOWN_TOP_LEVEL_KEYS.join(", ")
            ),
            rule: "unknown-top-level-key".to_owned(),
        }
    }

    /// A name defined in two files (a node, a preset, or a var).
    ///
    /// Both files are named, because "which one wins" is not a question this
    /// config system answers. It refuses — but as a **finding**, not a load
    /// failure: F0.1 forbids a new stop-fatal error, and someone who copies a node
    /// file while an environment is running still has to be able to tear it down.
    /// The last definition wins for the purposes of a `veld stop` that has to
    /// proceed anyway.
    pub(crate) fn duplicate_definition(kind: &str, name: &str, first: &str, second: &str) -> Self {
        Self {
            severity: Severity::Error,
            location: second.to_owned(),
            message: format!(
                "{kind} \"{name}\" is defined in two files: {first} and {second}. A {kind} is \
                 defined in exactly one file — there is deliberately no precedence rule to \
                 fall back on. Delete one, or rename it"
            ),
            rule: "duplicate-definition".to_owned(),
        }
    }

    /// An included file that could not be parsed.
    ///
    /// Reported rather than fatal, for the same F0.1 reason. "Never a silently
    /// absent node" still holds: the node is absent, but loudly — `veld start` and
    /// `veld lint` refuse and name the file.
    pub(crate) fn unparseable_include(location: &str, detail: &str) -> Self {
        Self {
            severity: Severity::Error,
            location: location.to_owned(),
            message: format!(
                "this included file could not be parsed, so the nodes it defines are \
                 missing: {detail}"
            ),
            rule: "unparseable-include".to_owned(),
        }
    }

    /// An included file that could not be read at all.
    pub(crate) fn unreadable_include(location: &str, detail: &str) -> Self {
        Self {
            severity: Severity::Error,
            location: location.to_owned(),
            message: format!(
                "this included file matched a glob but could not be read, so the nodes it \
                 defines are missing: {detail}"
            ),
            rule: "unreadable-include".to_owned(),
        }
    }

    /// `include` in a file that is not the root config.
    pub(crate) fn nested_include(location: &str) -> Self {
        Self {
            severity: Severity::Error,
            location: location.to_owned(),
            message: "only the root config may declare \"include\"; this one was ignored. \
                      Nested includes would make load order — and so error messages — \
                      depend on a graph nobody can see"
                .to_owned(),
            rule: "nested-include".to_owned(),
        }
    }

    /// A project-level singleton declared in an included file, where it is ignored.
    ///
    /// An error rather than a warning: the author wrote a value that has no effect,
    /// and every alternative is worse. Merging them would need a precedence rule
    /// ("which file's `url_template` wins?") that this config system deliberately
    /// refuses to have, and staying silent is how a team spends an afternoon
    /// wondering why their `proxy` block does nothing.
    pub(crate) fn root_only_key(key: &str, file: &str) -> Self {
        Self {
            severity: Severity::Error,
            location: format!("{file}:{key}"),
            message: format!(
                "\"{key}\" is a project-level setting, so it is only read from the root \
                 config file — the copy in {file} is ignored. Move it to the root file. \
                 Only `nodes`, `presets`, `vars`, `env`, `setup`, and `teardown` merge \
                 across files, because those are the ones with a per-key owner; a \
                 single value would need a precedence rule instead"
            ),
            rule: "root-only-key".to_owned(),
        }
    }

    fn notice(rule: &str, location: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Notice,
            location: location.into(),
            message: message.into(),
            rule: rule.to_owned(),
        }
    }
}

/// A single message summarising every `Error`-severity finding, or `None` when
/// there are none. This is what a command that must refuse to proceed prints.
pub fn error_summary(findings: &[Finding]) -> Option<String> {
    let errors: Vec<String> = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .map(|f| format!("  {}: {}", f.location, f.message))
        .collect();
    if errors.is_empty() {
        return None;
    }
    Some(format!(
        "{} problem(s) in veld.json:\n{}",
        errors.len(),
        errors.join("\n")
    ))
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
    ///
    /// An `IndexMap`, not a `HashMap`: declaration order is what unpinned preset
    /// keys are assigned from (see [`crate::presets`]), so throwing it away at
    /// parse time would make the number a user types depend on hash iteration
    /// order. It is also why the picker used to sort alphabetically — the only
    /// deterministic order a `HashMap` could offer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presets: Option<IndexMap<String, PresetDef>>,

    /// The preset `veld start` runs when given nothing to start.
    ///
    /// Answers "just start it" — the most common thing a human types and by far
    /// the most common thing a coding agent is asked to do — without a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_preset: Option<String>,

    /// Client-side log levels to capture (project-level default).
    /// Supported values: "log", "warn", "error", "info", "debug".
    /// "exception" is always captured regardless of this setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_log_levels: Option<Vec<String>>,

    /// Feature toggles (project-level defaults).
    /// Overridable at node and variant level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesConfig>,

    /// Reverse-proxy header rules (project-level defaults).
    /// Applied by the local Caddy proxy and the public web gateway.
    /// Overridable at node and variant level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,

    /// Global environment variables inherited by all node variants.
    /// Overridable at node and variant level; a more specific layer erases an
    /// inherited key with `"KEY": null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<NullableMap<ConfigValue>>,

    /// One definition point per value, referenced by name at every use site as
    /// `${vars.<name>}` (F4).
    ///
    /// **The key never leaves the node that uses it.** A var holds a *value*, not
    /// a config fragment: `${vars.db_url}` is written where `DATABASE_URL` is set,
    /// so `rg DATABASE_URL` still finds the line and a reader of that node still
    /// sees which keys it has. That is the difference between deduplicating values
    /// and deduplicating structure, and it is why a var is a
    /// [`ConfigValue`] — a scalar or a single value source, never an object, never
    /// a probe block, never an `env` map. If a body could be stored in a var this
    /// would be a template system.
    ///
    /// A var may not reference another var: one hop, always, so provenance is a
    /// single lookup (`veld config --why`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars: Option<HashMap<String, ConfigValue>>,

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

    /// **Reserved.** Repo-declared lifecycle hooks, keyed by event
    /// (`worktree.created`, `project.created`, `run.stopped`). Parsed into an
    /// opaque value, stored, and **not executed by this version** — `veld lint`
    /// says so.
    ///
    /// Reserved now so the desktop app's extension work does not later distort the
    /// node model. Hooks are **not nodes**: they never join the dependency graph,
    /// get no allocated port, and have no probes. If something needs readiness or
    /// a port, it is a node. And they are **repo-declared only** — a hook may
    /// never arrive from a fetched extension, because hooks run arbitrary code on
    /// a developer machine and keeping them in reviewed repo files is what
    /// preserves the no-remote-execution guarantee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<serde_json::Value>,

    /// **Reserved.** JSON-defined UI extensions for the desktop app and the IDE
    /// view. Parsed, stored, not executed. See [`Self::hooks`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<serde_json::Value>,

    /// Whether this config was assembled from more than one file.
    ///
    /// Only used to phrase diagnostics: "unknown node" has different likely causes
    /// in a split config (a file renamed out of an `include` glob) than in a single
    /// one (a typo), and the error should say the right thing.
    #[serde(skip)]
    pub loaded_from_multiple_files: bool,

    /// Problems found while *parsing* that must not fail the load.
    ///
    /// Some defects are only visible in the raw text — a duplicate key, which
    /// `serde_json` silently resolves last-wins, is the motivating case. They
    /// still have to be errors (F1), but making them *load* errors would put a
    /// new failure on the path `veld stop` / `status` / `logs` and the daemon
    /// monitor use, and F0.1 forbids exactly that: `on_stop` is read from the
    /// on-disk config at stop time, so a config that will not load means
    /// teardown never runs and containers leak with no way to clean up.
    ///
    /// So the loader records them here and [`validate`] reports them.
    /// `veld start` and `veld lint` refuse; everything else keeps working on the
    /// last-wins interpretation, which is what it did before this existed.
    #[serde(skip)]
    pub deferred_findings: Vec<Finding>,
}

// ---------------------------------------------------------------------------
// Value sources and sensitivity (F7)
// ---------------------------------------------------------------------------

/// A configured value: **where it comes from**, and **whether it is sensitive**.
///
/// A plain string is a literal, non-secret value — that is the common case and it
/// stays terse. The object form names exactly one source plus an optional
/// `secret` flag:
///
/// ```jsonc
/// "env": {
///   "REGION":       "eu-central-1",
///   "PG_PASSWORD":  { "value": "devpassword", "secret": true },
///   "GITHUB_TOKEN": { "env": "GITHUB_TOKEN", "secret": true },
///   "SIGNING_KEY":  { "file": ".secrets/signing.key", "secret": true },
///   "DATABASE_URL": { "argv": ["secret-tool", "read", "path/to/secret"], "secret": true }
/// }
/// ```
///
/// **veld never takes custody of a secret.** It carries a *pointer* to one and a
/// *sensitivity flag*; resolution happens at run start and the resolved value
/// goes to the child process's environment (or a file — F9.1) and nowhere else.
/// There is no provider table and no vendor name anywhere in the schema:
/// `argv` runs a command and reads its stdout, and which command that is remains
/// the author's business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValue {
    /// Where the value is read from.
    pub source: SecretSource,
    /// Declared sensitivity. Not a Rust newtype: `sensitive_outputs` and the
    /// share-manifest wire types must stay serde-lenient, so this is a flag that
    /// travels with the value rather than a type that forbids `Serialize`.
    pub secret: bool,
}

impl ConfigValue {
    /// A plain, non-secret literal — the string form.
    pub fn literal(value: impl Into<String>) -> Self {
        Self {
            source: SecretSource::Literal(value.into()),
            secret: false,
        }
    }

    /// The literal text, if this value is inline. `None` for every form that has
    /// to be *fetched* — those are only known after resolution at run start.
    pub fn as_literal(&self) -> Option<&str> {
        match &self.source {
            SecretSource::Literal(v) => Some(v),
            _ => None,
        }
    }

    /// A short description of the source for diagnostics. Never includes a
    /// literal value, so it is safe to log.
    pub fn source_label(&self) -> String {
        match &self.source {
            SecretSource::Literal(_) => "an inline value".to_owned(),
            SecretSource::Env(name) => format!("environment variable {name}"),
            SecretSource::File(path) => format!("file {path}"),
            SecretSource::Command(c) | SecretSource::Shell(c) => format!("command `{c}`"),
            SecretSource::Argv(a) => format!("command {a:?}"),
        }
    }
}

impl Serialize for ConfigValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        // The terse form round-trips when there is nothing else to say.
        if !self.secret {
            if let SecretSource::Literal(v) = &self.source {
                return s.serialize_str(v);
            }
        }
        let mut m = s.serialize_map(Some(2))?;
        match &self.source {
            SecretSource::Literal(v) => m.serialize_entry("value", v)?,
            SecretSource::Env(v) => m.serialize_entry("env", v)?,
            SecretSource::File(v) => m.serialize_entry("file", v)?,
            SecretSource::Command(v) => m.serialize_entry("command", v)?,
            SecretSource::Argv(v) => m.serialize_entry("argv", v)?,
            SecretSource::Shell(v) => m.serialize_entry("shell", v)?,
        }
        if self.secret {
            m.serialize_entry("secret", &true)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for ConfigValue {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) => Ok(ConfigValue::literal(s)),
            serde_json::Value::Object(mut map) => {
                let secret = match map.remove("secret") {
                    None => false,
                    Some(serde_json::Value::Bool(b)) => b,
                    Some(other) => {
                        return Err(D::Error::custom(format!(
                            "\"secret\" must be true or false, got {other}"
                        )));
                    }
                };
                if map.len() != 1 {
                    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
                    keys.sort();
                    return Err(D::Error::custom(format!(
                        "a value needs exactly one source key — \"value\", \"env\", \"file\", \
                         \"argv\", or \"shell\" — found {}",
                        if keys.is_empty() {
                            "none".to_owned()
                        } else {
                            keys.join(", ")
                        }
                    )));
                }
                let (key, val) = map.into_iter().next().expect("len checked == 1");
                let source = match key.as_str() {
                    // `value` is the object spelling of a literal: it exists so an
                    // inline literal can still carry `secret: true`.
                    "value" => SecretSource::Literal(
                        val.as_str()
                            .ok_or_else(|| D::Error::custom("\"value\" must be a string"))?
                            .to_owned(),
                    ),
                    _ => secret_source_from_value(serde_json::json!({ key: val }))
                        .map_err(D::Error::custom)?,
                };
                Ok(ConfigValue { source, secret })
            }
            other => Err(D::Error::custom(format!(
                "a value must be a string or a single-source object, got {other}"
            ))),
        }
    }
}

/// A value delivered to **disk** before the process starts (F9.1).
///
/// The docs have long said a secret may reach a process via the environment *or a
/// file*, but there was no syntax for the second half — so the workaround was a
/// shell command that writes the secret itself, which puts it in the process table
/// on the way (exactly what `secret: true` exists to prevent).
///
/// ```jsonc
/// "files": {
///   ".secrets/pg.pem": { "env": "PG_CLIENT_CERT", "secret": true, "mode": "0400" }
/// }
/// ```
///
/// The path is resolved from the project root, like every other relative path.
/// `mode` defaults to `0600`: the motivating case is a credential, and a
/// world-readable default would undo the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDelivery {
    /// Where the content comes from — the same value sources as everything else.
    pub value: ConfigValue,
    /// Octal file mode as written (e.g. `"0600"`).
    pub mode: Option<String>,
}

/// Default mode for a delivered file: owner read/write only.
pub const DEFAULT_FILE_MODE: u32 = 0o600;

impl FileDelivery {
    /// The mode to create the file with, or an error naming the bad value.
    ///
    /// Parsed as octal whether or not it carries a leading `0`, because `"600"`
    /// and `"0600"` obviously mean the same thing to the person writing it — and
    /// reading `"600"` as *decimal* 600 would silently produce mode 1130.
    pub fn parsed_mode(&self) -> Result<u32, String> {
        let Some(raw) = &self.mode else {
            return Ok(DEFAULT_FILE_MODE);
        };
        let digits = raw.strip_prefix("0o").unwrap_or(raw);
        u32::from_str_radix(digits, 8)
            .ok()
            .filter(|m| *m <= 0o7777)
            .ok_or_else(|| {
                format!("\"{raw}\" is not a file mode; write it in octal, e.g. \"0600\"")
            })
    }
}

impl Serialize for FileDelivery {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut m = s.serialize_map(None)?;
        match &self.value.source {
            SecretSource::Literal(v) => m.serialize_entry("value", v)?,
            SecretSource::Env(v) => m.serialize_entry("env", v)?,
            SecretSource::File(v) => m.serialize_entry("file", v)?,
            SecretSource::Command(v) => m.serialize_entry("command", v)?,
            SecretSource::Argv(v) => m.serialize_entry("argv", v)?,
            SecretSource::Shell(v) => m.serialize_entry("shell", v)?,
        }
        if self.value.secret {
            m.serialize_entry("secret", &true)?;
        }
        if let Some(mode) = &self.mode {
            m.serialize_entry("mode", mode)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for FileDelivery {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut map = match serde_json::Value::deserialize(d)? {
            serde_json::Value::Object(map) => map,
            other => {
                return Err(D::Error::custom(format!(
                    "a delivered file must be an object with one source key and an \
                     optional \"mode\", got {other}"
                )));
            }
        };
        let mode = match map.remove("mode") {
            None => None,
            Some(serde_json::Value::String(s)) => Some(s),
            // A bare number would already have lost its leading zero, so the
            // octal/decimal ambiguity is settled by requiring a string.
            Some(other) => {
                return Err(D::Error::custom(format!(
                    "\"mode\" must be a string like \"0600\", not {other}"
                )));
            }
        };
        let value: ConfigValue =
            serde_json::from_value(serde_json::Value::Object(map)).map_err(D::Error::custom)?;
        let delivery = FileDelivery { value, mode };
        // Fail on a bad mode here rather than at spawn time, when the run is
        // already half up.
        delivery.parsed_mode().map_err(D::Error::custom)?;
        Ok(delivery)
    }
}

// ---------------------------------------------------------------------------
// Node-level defaults: maps, and how a variant removes an inherited key
// ---------------------------------------------------------------------------

/// A map whose values may be `null` to mean **remove the inherited key**.
///
/// Node-level defaults are additive per key (project → node → variant), so
/// without this a variant could override an inherited entry but never get rid of
/// one. `"KEY": null` is that eraser. The `None` entries are dropped by the
/// `resolve_*` function after merging, so nothing downstream ever sees one.
pub type NullableMap<T> = HashMap<String, Option<T>>;

/// Merge `project → node → variant` layers of a [`NullableMap`], most specific
/// last, then drop the keys a layer erased with `null`.
///
/// This is the **per-key override** strategy — one of the three distinct merge
/// strategies in the resolved config (see the merge table in
/// `docs/configuration.md`). It is used by `env`, `ports`, and `depends_on`.
/// Do not try to unify it with the others: `features` is per *field*, and
/// `probes`/`share`/`outputs` replace wholesale, deliberately.
fn merge_nullable_maps<T: Clone>(
    layers: [Option<&NullableMap<T>>; 3],
) -> Option<HashMap<String, T>> {
    if layers.iter().all(|l| l.is_none()) {
        return None;
    }
    let mut merged: NullableMap<T> = HashMap::new();
    for layer in layers.into_iter().flatten() {
        for (k, v) in layer {
            merged.insert(k.clone(), v.clone());
        }
    }
    Some(
        merged
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect(),
    )
}

/// Distinguish "key absent" from "key present and `null`".
///
/// `Option<Option<T>>` after this: `None` = absent (inherit), `Some(None)` =
/// explicitly `null` (erase the inherited value), `Some(Some(v))` = override.
/// serde cannot express that with `Option<T>` alone — both cases deserialize to
/// `None` — so every field a variant may erase needs this.
fn explicit_null<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// How a named port is obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortSpec {
    /// veld allocates a free port and reports it as `${veld.ports.<name>}`.
    Auto,
    /// A fixed port. Discouraged — a literal port silently breaks parallel
    /// worktrees, which is the whole reason named auto-ports exist.
    Fixed(u16),
}

impl Serialize for PortSpec {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            PortSpec::Auto => s.serialize_str("auto"),
            PortSpec::Fixed(p) => s.serialize_u16(*p),
        }
    }
}

impl<'de> Deserialize<'de> for PortSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) if s == "auto" => Ok(PortSpec::Auto),
            serde_json::Value::Number(n) => n
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .filter(|p| *p > 0)
                .map(PortSpec::Fixed)
                .ok_or_else(|| D::Error::custom("a fixed port must be between 1 and 65535")),
            other => Err(D::Error::custom(format!(
                "a port must be \"auto\" or a number, got {other}"
            ))),
        }
    }
}

/// The name of the port `${veld.port}` refers to when a node declares several.
pub const PRIMARY_PORT_NAME: &str = "http";

/// Resolved named ports for one node, and which of them is primary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPorts {
    /// Port name → spec, in declaration-independent (sorted) order.
    pub ports: BTreeMap<String, PortSpec>,
    /// The name `${veld.port}` aliases.
    pub primary: String,
}

impl ResolvedPorts {
    /// Pick the primary port: the one named `http`, or the sole entry when only
    /// one is declared. Ambiguity is a validation error rather than a guess —
    /// silently picking alphabetically would make `${veld.port}` mean whichever
    /// name happened to sort first.
    fn choose_primary(ports: &BTreeMap<String, PortSpec>) -> Option<String> {
        if ports.contains_key(PRIMARY_PORT_NAME) {
            return Some(PRIMARY_PORT_NAME.to_owned());
        }
        if ports.len() == 1 {
            return ports.keys().next().cloned();
        }
        None
    }
}

/// Resolve a node's named ports (`project` has none; node → variant, per key).
///
/// Returns `None` when nothing is declared — a `start_server` with no `ports`
/// then behaves exactly as it always has: one allocated port, reachable as
/// `${veld.port}`.
pub fn resolve_ports(
    node: Option<&NullableMap<PortSpec>>,
    variant: Option<&NullableMap<PortSpec>>,
) -> Option<ResolvedPorts> {
    let merged = merge_nullable_maps([None, node, variant])?;
    if merged.is_empty() {
        return None;
    }
    let ports: BTreeMap<String, PortSpec> = merged.into_iter().collect();
    let primary = ResolvedPorts::choose_primary(&ports).unwrap_or_default();
    Some(ResolvedPorts { ports, primary })
}

/// Resolve `depends_on` (node → variant, per key; `null` erases).
pub fn resolve_depends_on(
    node: Option<&NullableMap<String>>,
    variant: Option<&NullableMap<String>>,
) -> Option<HashMap<String, String>> {
    merge_nullable_maps([None, node, variant])
}

// ---------------------------------------------------------------------------
// Commands — one vocabulary for every place veld runs something
// ---------------------------------------------------------------------------

/// One command veld runs, in exactly one of two forms.
///
/// | Form | Meaning |
/// |---|---|
/// | `argv` | An array, spawned directly. No shell, no word splitting, no globbing. |
/// | `shell` | A string, run via `sh -c`. The author owns quoting. |
///
/// **The argv guarantee:** interpolation runs per element, *after* the array is
/// fixed (see [`CommandSpec::interpolate`]), so a value containing spaces, globs,
/// quotes, or newlines can never change the argument count. That is the whole
/// point of the form — `["psql", "${vars.db_url}"]` stays two arguments whatever
/// the URL contains.
///
/// `shell` is not a deprecated fallback: it is permanently supported, and it is
/// what makes this a safe breaking change. Any node that misbehaves under `argv`
/// can be reverted to a string by its author with no veld change and no config
/// version change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSpec {
    Argv(Vec<String>),
    Shell(String),
}

impl CommandSpec {
    /// Resolve the effective command from the keys a carrier object may set.
    ///
    /// `argv` wins over `shell` wins over the legacy `command`, deterministically
    /// — declaring more than one is a validation error
    /// ([`check_exactly_one_command`]), and this ordering only decides what runs
    /// while the author still has an invalid config in front of them.
    pub fn from_keys(keys: &CommandKeys) -> Option<Self> {
        if let Some(argv) = &keys.argv {
            return Some(CommandSpec::Argv(argv.clone()));
        }
        if let Some(shell) = &keys.shell {
            return Some(CommandSpec::Shell(shell.clone()));
        }
        keys.command.clone().map(CommandSpec::Shell)
    }

    /// Interpolate `${…}` references. For `argv`, **per element** — the element
    /// count is fixed before any value is substituted.
    pub fn interpolate(
        &self,
        ctx: &crate::variables::VariableContext,
    ) -> Result<Self, crate::variables::VariableError> {
        match self {
            CommandSpec::Argv(argv) => argv
                .iter()
                .map(|a| crate::variables::interpolate(a, ctx))
                .collect::<Result<Vec<_>, _>>()
                .map(CommandSpec::Argv),
            CommandSpec::Shell(s) => crate::variables::interpolate(s, ctx).map(CommandSpec::Shell),
        }
    }

    /// A human-readable rendering for logs, progress lines, and error messages.
    /// Never re-parsed — it is display only, and an `argv` is shown in its list
    /// form precisely so `["a", "b c"]` does not read like `a b c`.
    pub fn display(&self) -> String {
        match self {
            CommandSpec::Argv(argv) => format!("{argv:?}"),
            CommandSpec::Shell(s) => s.clone(),
        }
    }

    /// True when there is nothing to run (an empty argv or an empty string).
    pub fn is_empty(&self) -> bool {
        match self {
            CommandSpec::Argv(argv) => argv.iter().all(|a| a.is_empty()) || argv.is_empty(),
            CommandSpec::Shell(s) => s.trim().is_empty(),
        }
    }

    /// A `shell` command wrapping a script file — how `script:` entries run.
    pub fn script(path: &Path) -> Self {
        CommandSpec::Shell(format!("sh {}", path.display()))
    }
}

impl Serialize for CommandSpec {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut m = s.serialize_map(Some(1))?;
        match self {
            // Always the object form, even for a command that was written as a
            // bare v1/v2 string: the deserializer accepts both, so a round-trip
            // stays loadable by every schema version, and only the object form
            // is valid in v3.
            CommandSpec::Argv(argv) => m.serialize_entry("argv", argv)?,
            CommandSpec::Shell(sh) => m.serialize_entry("shell", sh)?,
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for CommandSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        match serde_json::Value::deserialize(d)? {
            // The v1/v2 form: a bare shell string. Deserializing it still succeeds
            // so that `reject_v3_legacy_commands` is the thing that rejects it —
            // that gate names every position and the replacement form, where a
            // serde error here would only say "invalid type: string".
            serde_json::Value::String(s) => Ok(CommandSpec::Shell(s)),
            serde_json::Value::Object(map) => {
                let keys: CommandKeys = serde_json::from_value(serde_json::Value::Object(map))
                    .map_err(D::Error::custom)?;
                let set = keys.count_set();
                if set == 0 {
                    return Err(D::Error::custom(
                        "a command needs exactly one of \"argv\" (array) or \"shell\" (string)",
                    ));
                }
                if set > 1 {
                    return Err(D::Error::custom(format!(
                        "a command must set exactly one of \"argv\" or \"shell\", found {set}"
                    )));
                }
                Ok(CommandSpec::from_keys(&keys).expect("exactly one key is set"))
            }
            _ => Err(D::Error::custom(
                "a command must be an { argv } or { shell } object",
            )),
        }
    }
}

/// The command keys as they appear side by side on whatever object carries a
/// command — a variant, a setup step, an action, a probe, a value source.
///
/// Flattened into each carrier rather than nested, because "two keys, exactly
/// one of them" is the vocabulary: a variant *is* the thing that runs, so it
/// carries `argv`/`shell` directly rather than wrapping them in another object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandKeys {
    /// Argument vector, spawned directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,

    /// Shell string, run via `sh -c`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,

    /// The v1/v2 shell string. **Removed in `schemaVersion: "3"`**, which is the
    /// only version veld reads — so this field exists purely so that
    /// [`reject_v3_legacy_commands`] owns the rejection and can name every
    /// position and its replacement, instead of serde reporting an unknown key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl CommandKeys {
    /// How many of the three mutually-exclusive keys are set.
    pub fn count_set(&self) -> usize {
        self.argv.is_some() as usize
            + self.shell.is_some() as usize
            + self.command.is_some() as usize
    }

    /// The effective command, if any.
    pub fn spec(&self) -> Option<CommandSpec> {
        CommandSpec::from_keys(self)
    }

    /// Names of the keys that are set, for error messages.
    fn set_names(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.argv.is_some() {
            v.push("argv");
        }
        if self.shell.is_some() {
            v.push("shell");
        }
        if self.command.is_some() {
            v.push("command");
        }
        v
    }
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

    /// The command to execute: `argv` or `shell` (or the v1/v2 `command`).
    #[serde(flatten)]
    pub cmd: CommandKeys,

    /// Optional message shown when the command fails (non-zero exit).
    /// Primarily useful for setup steps that validate prerequisites.
    #[serde(
        rename = "failureMessage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub failure_message: Option<String>,
}

pub(crate) fn default_url_template() -> String {
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

    /// The public web gateway this environment points at. Only needed for
    /// services that `expose` `web`. A bare URL string, or `{ "url", "token" }`
    /// carrying the gateway's registration auth token as a [`SecretSource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<GatewayRef>,

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
/// - `{ "shell": "op read op://vault/relay/token" }` — run a shell command and
///   use its stdout (1Password / Vault / any secret-manager CLI). Runs with the
///   user's login-shell `PATH` ([`crate::user_path`]) so those CLIs are found
///   even though resolution happens on a daemon with a bare service `PATH`.
///
/// Resolution (running the command, reading the file/env) happens in the daemon
/// at share time, not in this crate — this type only carries the declaration.
///
/// Adding a variant here means updating, in lockstep: `Serialize` /
/// `secret_source_from_value` below (deserialize is a catch-all `Err`, so a new
/// variant compiles + serializes but *silently fails to parse* until added),
/// the `Debug` redaction below, `resolve_secret` in `veld-share`
/// (`endpoint.rs`, which resolves `Command` via [`crate::user_path`]), and the
/// `SecretSource` `$def` in
/// `schema/v3/veld.schema.json` (hand-maintained — no compiler check ties it to
/// this enum).
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum SecretSource {
    /// The literal secret value, inline in config.
    Literal(String),
    /// Name of an environment variable holding the secret.
    Env(String),
    /// Path to a file whose (trimmed) contents are the secret.
    File(String),
    /// A shell command whose (trimmed) stdout is the secret — the v1/v2 `command`
    /// key. Rejected in v3 documents, which use `argv`/`shell` like everywhere
    /// else.
    Command(String),
    /// An argument vector, spawned directly; its (trimmed) stdout is the secret.
    Argv(Vec<String>),
    /// A shell string; its (trimmed) stdout is the secret.
    Shell(String),
}

impl SecretSource {
    /// The command this source runs, if it runs one. `None` for the
    /// literal/env/file forms, which read a value rather than execute anything.
    pub fn command(&self) -> Option<CommandSpec> {
        match self {
            SecretSource::Command(s) | SecretSource::Shell(s) => {
                Some(CommandSpec::Shell(s.clone()))
            }
            SecretSource::Argv(argv) => Some(CommandSpec::Argv(argv.clone())),
            _ => None,
        }
    }
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
            SecretSource::Argv(a) => f.debug_tuple("Argv").field(a).finish(),
            SecretSource::Shell(c) => f.debug_tuple("Shell").field(c).finish(),
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
            SecretSource::Argv(v) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("argv", v)?;
                m.end()
            }
            SecretSource::Shell(v) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("shell", v)?;
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
                    "token object must have exactly one of \"env\", \"file\", \"argv\", or \
                     \"shell\""
                        .to_owned(),
                );
            }
            let (key, val) = map.into_iter().next().expect("len checked == 1");
            // `argv` is the one array-valued source, so it is read before the
            // shared string coercion below.
            if key == "argv" {
                let arr = val
                    .as_array()
                    .ok_or("token \"argv\" must be an array of strings")?;
                let argv: Vec<String> = arr
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "token \"argv\" must contain only strings".to_owned())
                    })
                    .collect::<Result<_, _>>()?;
                if argv.is_empty() {
                    return Err("token \"argv\" must not be empty".to_owned());
                }
                return Ok(SecretSource::Argv(argv));
            }
            let s = val
                .as_str()
                .ok_or_else(|| format!("token \"{key}\" must be a string"))?
                .to_owned();
            match key.as_str() {
                "env" => Ok(SecretSource::Env(s)),
                "file" => Ok(SecretSource::File(s)),
                "shell" => Ok(SecretSource::Shell(s)),
                // The v1/v2 spelling of `shell`.
                "command" => Ok(SecretSource::Command(s)),
                other => Err(format!(
                    "unknown token source \"{other}\"; expected \"env\", \"file\", \"argv\", or \
                     \"shell\""
                )),
            }
        }
        _ => Err("token must be a string or an { env | file | argv | shell } object".to_owned()),
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

/// A reference to the public web gateway an environment registers `web`
/// shares with. Mirrors [`RelayEntry`]'s serde shape: a
/// bare URL string round-trips, and the object form adds the registration
/// auth token the gateway requires.
#[derive(Clone, PartialEq, Eq)]
pub struct GatewayRef {
    /// Gateway base URL, e.g. `https://share.acme.internal`.
    pub url: String,
    /// Registration auth token source. The gateway always requires one; it may
    /// alternatively come from the `VELD_SHARE_GATEWAY_TOKEN` env override.
    pub token: Option<SecretSource>,
}

impl std::fmt::Debug for GatewayRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Delegates the token field to `SecretSource`'s redacting Debug.
        f.debug_struct("GatewayRef")
            .field("url", &self.url)
            .field("token", &self.token)
            .finish()
    }
}

impl Serialize for GatewayRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        match &self.token {
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

impl<'de> Deserialize<'de> for GatewayRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(url) => Ok(GatewayRef { url, token: None }),
            serde_json::Value::Object(mut map) => {
                let url = map
                    .remove("url")
                    .ok_or_else(|| D::Error::custom("gateway object must have a \"url\""))?;
                let url = url
                    .as_str()
                    .ok_or_else(|| D::Error::custom("gateway \"url\" must be a string"))?
                    .to_owned();
                let token = match map.remove("token") {
                    Some(t) => Some(secret_source_from_value(t).map_err(D::Error::custom)?),
                    None => None,
                };
                if !map.is_empty() {
                    let unknown: Vec<&str> = map.keys().map(String::as_str).collect();
                    return Err(D::Error::custom(format!(
                        "unknown key(s) in gateway: {}; expected \"url\" and optional \"token\"",
                        unknown.join(", ")
                    )));
                }
                Ok(GatewayRef { url, token })
            }
            _ => Err(D::Error::custom(
                "gateway must be a URL string or a { url, token } object",
            )),
        }
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
    /// Web-audience settings. Absent means defaults —
    /// which for `access` is `password` (never an open URL by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebSharePolicy>,
}

impl SharePolicy {
    /// Whether this policy permits the given audience.
    pub fn allows(&self, mode: ExposeMode) -> bool {
        self.expose.contains(&mode)
    }

    /// The explicitly configured web access mode, if any. `None` means the
    /// config is silent — the daemon then applies the CLI flag or the
    /// password-by-default posture. An explicit value always wins over
    /// runtime flags (config is the compliance surface).
    pub fn web_access(&self) -> Option<WebAccessMode> {
        self.web.as_ref().and_then(|w| w.access)
    }
}

/// Per-variant web-audience settings (`share.web`).
///
/// `deny_unknown_fields`: the natural-but-wrong config (e.g. a `"password":
/// "…"` key — the password is generated or `--password`, never config) must
/// fail loudly instead of being silently dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSharePolicy {
    /// Viewer access control for this service's public URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<WebAccessMode>,
}

/// How a browser viewer is admitted to a public (web) share URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebAccessMode {
    /// The gateway requires the share password before serving (default).
    Password,
    /// Anyone with the link is served (the unguessable slug is the only gate).
    Link,
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

    /// Reverse-proxy header rules override for all variants of this node.
    /// Overrides project-level `proxy`. Overridable at variant level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,

    /// Extra environment variables inherited by all variants of this node.
    /// Overrides project-level env. Overridable at variant level; `"KEY": null`
    /// erases an inherited key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<NullableMap<ConfigValue>>,

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

    // -- Node-level defaults (F3) ------------------------------------------
    //
    // A node may declare any of these once, here, and *any variant may override
    // it*. This deduplicates **values**, never structure: which keys a node has
    // is still written in that node, and `rg <ENV_VAR_NAME>` still finds the line
    // that sets it. There is no inheritance, no mixins, no templates — a variant
    // body is never assembled from somewhere else.
    //
    // The merge strategy differs per field on purpose; see the merge table in
    // `docs/configuration.md` and the `resolve_*` family below.
    /// Default step type for variants that do not state one.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub step_type: Option<StepType>,

    /// Default command for all variants of this node (`argv` / `shell`).
    #[serde(flatten)]
    pub cmd: CommandKeys,

    /// Default probes. **Replaced per probe**, not merged field-by-field: a probe
    /// is a tagged union, so field-wise merging would let a variant switch
    /// `type: "http"` to `type: "command"` and silently inherit a stale `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probes: Option<ProbesConfig>,

    /// Default sharing opt-in. Replaced wholesale by a variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<SharePolicy>,

    /// Default named ports. Additive per key; `"name": null` erases one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<NullableMap<PortSpec>>,

    /// Default dependencies. Additive per key; `"node": null` erases one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<NullableMap<String>>,

    /// Default outputs declaration. Replaced wholesale by a variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Outputs>,

    /// Default teardown hook. Replaced by a variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_stop: Option<CommandSpec>,

    /// Default values delivered to disk (F9.1). Additive per path; `"path": null`
    /// erases an inherited one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<NullableMap<FileDelivery>>,

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

    /// The command to execute: `argv` or `shell` (or the v1/v2 `command`).
    /// Supports `${veld.*}`, `${output.KEY}` (this node's live outputs),
    /// `${param.KEY}` (this action's parameters), and `${nodes.name.field}`
    /// substitution. The same values are also exported as environment variables
    /// (`$KEY` for outputs and parameters), so shell-style references work too —
    /// in a `shell` command. Under `argv` they do not, since there is no shell to
    /// expand them; use `${param.KEY}` there.
    #[serde(flatten)]
    pub cmd: CommandKeys,

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
    /// Step type: `command` or `start_server`. Optional when the node declares
    /// one; a variant that states it wins.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub step_type: Option<StepType>,

    /// What this variant runs: `argv` or `shell` (or the v1/v2 `command`).
    /// A variant *is* the thing that runs, so the keys sit on it directly.
    #[serde(flatten)]
    pub cmd: CommandKeys,

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

    /// Dependencies: node name -> variant name. Additive over the node-level
    /// map; `"node": null` erases an inherited dependency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<NullableMap<String>>,

    /// Extra environment variables injected into the process. `"KEY": null`
    /// erases a key inherited from the node or project level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<NullableMap<ConfigValue>>,

    /// Values delivered to disk before this variant starts (F9.1). Additive over
    /// the node-level map; `"path": null` erases an inherited one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<NullableMap<FileDelivery>>,

    /// Named ports veld allocates for this variant, additive over the
    /// node-level map. `"name": null` erases an inherited one. Absent means the
    /// pre-`ports` behaviour: a `start_server` gets exactly one allocated port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<NullableMap<PortSpec>>,

    /// Outputs declaration. **Replaces** the node-level one wholesale; `null`
    /// erases it.
    ///
    /// - For `command`: a list of declared output names (`Vec<String>`).
    /// - For `start_server`: a map of synthetic outputs.
    #[serde(
        default,
        deserialize_with = "explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub outputs: Option<Option<Outputs>>,

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
    pub skip_if: Option<CommandSpec>,

    /// Optional URL template override for this specific variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_template: Option<String>,

    /// Teardown command to run when the environment is stopped.
    /// Executed in reverse dependency order during `veld stop`.
    ///
    /// **Replaces** the node-level hook; `null` erases it. `null` has to be
    /// distinguishable from absent here: with a plain `Option` an author writing
    /// `"on_stop": null` to *disable* the node's hook got the node's hook anyway,
    /// and it ran. Of all the fields to silently ignore an opt-out on, the one that
    /// executes a command during teardown is the worst.
    #[serde(
        default,
        deserialize_with = "explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub on_stop: Option<Option<CommandSpec>>,

    /// Client-side log levels override for this specific variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_log_levels: Option<Vec<String>>,

    /// Feature toggles override for this specific variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesConfig>,

    /// Reverse-proxy header rules override for this specific variant.
    /// Overrides node- and project-level `proxy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,

    /// Working directory for this variant. Relative paths are resolved from the project root (the directory containing veld.json).
    /// Overrides node-level `cwd`. Supports variable substitution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Sharing opt-in for this variant. Absent (or an empty `expose` list) means
    /// this service can never be shared — `veld share` refuses it. This is the
    /// explicit, per-service consent that makes sharing auditable.
    ///
    /// **Replaces** the node-level policy wholesale rather than merging: sharing
    /// is a consent decision, and a half-inherited `expose` list is exactly the
    /// kind of surprise it must not have. `null` erases the node-level opt-in.
    #[serde(
        default,
        deserialize_with = "explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub share: Option<Option<SharePolicy>>,
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
/// variant > node > project. For each key, the most specific layer wins, and a
/// layer erases an inherited key by setting it to `null`.
pub fn resolve_env(
    project: Option<&NullableMap<ConfigValue>>,
    node: Option<&NullableMap<ConfigValue>>,
    variant: Option<&NullableMap<ConfigValue>>,
) -> Option<HashMap<String, ConfigValue>> {
    merge_nullable_maps([project, node, variant])
}

// ---------------------------------------------------------------------------
// Proxy header rules
// ---------------------------------------------------------------------------

/// Static header manipulation applied by the reverse proxies (local Caddy +
/// public web gateway) to requests forwarded upstream and responses returned to
/// the client. Absent = the proxies pass headers through with only their
/// intrinsic, correctness-required rewrites (see the gateway/Caddy proxy docs).
///
/// This is the generic escape hatch for framework quirks — most notably Next.js
/// dev servers that gate WebSocket HMR on the `Origin` header. The preferred fix
/// there is the framework's own `allowedDevOrigins`; `proxy.request.remove:
/// ["Origin"]` is the sledgehammer for frameworks that offer no allow-list.
///
/// Resolvable at project, node, and variant level (most specific wins; see
/// [`resolve_proxy`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    /// Rules applied to the request forwarded to the upstream service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<HeaderRules>,

    /// Rules applied to the response returned from the upstream service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<HeaderRules>,
}

/// A set of header manipulations: names to remove, and static name→value pairs
/// to set (overwriting any existing value). Header names are matched
/// case-insensitively by the proxies; `set` uses a single value per name.
///
/// Deliberately NOT `deny_unknown_fields`: this type is also the wire payload
/// embedded in [`ResolvedProxy`] → `SharedNode` → the share manifest, which is
/// deserialized by separately-versioned receivers (the web gateway, join-side
/// daemons). Strict field rejection here would make a future field addition
/// fail the *entire* manifest on an older receiver. Structural typos are caught
/// one level up by `ProxyConfig`'s `deny_unknown_fields` plus the JSON schema.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderRules {
    /// Header names to remove before forwarding.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,

    /// Header name → value pairs to set (replacing any existing value).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
}

impl HeaderRules {
    /// True when there is nothing to do (no removes and no sets).
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.set.is_empty()
    }
}

/// Resolved (concrete) proxy header rules for one node — no more `Option`s.
/// This is what travels the wire to the gateway (in the share manifest) and to
/// the helper (in the Caddy route), so it derives `Serialize`/`Deserialize`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedProxy {
    #[serde(default, skip_serializing_if = "HeaderRules::is_empty")]
    pub request: HeaderRules,
    #[serde(default, skip_serializing_if = "HeaderRules::is_empty")]
    pub response: HeaderRules,
}

impl ResolvedProxy {
    /// True when neither request nor response rules do anything.
    pub fn is_empty(&self) -> bool {
        self.request.is_empty() && self.response.is_empty()
    }
}

/// Merge two `HeaderRules` layers, most-specific last. Header names are treated
/// case-insensitively throughout (HTTP header names are case-insensitive):
/// `remove` lists union (first spelling wins), and `set` maps override per key
/// with the more specific layer winning — including when the two layers spell
/// the same header with different case.
fn merge_header_rules(base: HeaderRules, over: &HeaderRules) -> HeaderRules {
    let mut remove = base.remove;
    for name in &over.remove {
        if !remove.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            remove.push(name.clone());
        }
    }
    let mut set = base.set;
    for (k, v) in &over.set {
        // Drop any existing key that differs only by case, so the more specific
        // layer's spelling+value wins deterministically (a BTreeMap keyed by raw
        // string would otherwise keep both "X-Foo" and "x-foo").
        set.retain(|existing, _| !existing.eq_ignore_ascii_case(k));
        set.insert(k.clone(), v.clone());
    }
    HeaderRules { remove, set }
}

/// Resolve proxy header rules by layering project → node → variant, most
/// specific last. Within each of `request`/`response`, `remove` lists union and
/// `set` maps merge (variant > node > project per key), both case-insensitively.
pub fn resolve_proxy(
    project: Option<&ProxyConfig>,
    node: Option<&ProxyConfig>,
    variant: Option<&ProxyConfig>,
) -> ResolvedProxy {
    let layers: [Option<&ProxyConfig>; 3] = [project, node, variant];
    let mut request = HeaderRules::default();
    let mut response = HeaderRules::default();
    for layer in layers.into_iter().flatten() {
        if let Some(rules) = &layer.request {
            request = merge_header_rules(request, rules);
        }
        if let Some(rules) = &layer.response {
            response = merge_header_rules(response, rules);
        }
    }
    // A header named in both `remove` and `set` is contradictory; `set` wins
    // (you asked for a concrete value). Resolve it HERE so both proxies agree —
    // the gateway applies remove-then-set (set wins) but Caddy's emitted
    // delete+set would otherwise resolve by Caddy's own op order. Dropping the
    // overlap from `remove` makes the outcome identical on both.
    request
        .remove
        .retain(|n| !request.set.keys().any(|k| k.eq_ignore_ascii_case(n)));
    response
        .remove
        .retain(|n| !response.set.keys().any(|k| k.eq_ignore_ascii_case(n)));
    ResolvedProxy { request, response }
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

    /// The command run for `type: "command"`: `argv` or `shell` (or the v1/v2
    /// `command`).
    #[serde(flatten)]
    pub cmd: CommandKeys,

    /// Which named port from the node's `ports` map this probe checks. Absent
    /// means the primary port, which is what a single-port node has always used.
    ///
    /// Needed because a multi-port node's readiness is rarely "any port is open":
    /// a debug-adapter variant's debugger port opens immediately while the
    /// application port is still binding, so probing the wrong one reports ready
    /// too early.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,

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
    ///
    /// A variant **replaces** the whole probe object rather than merging into it;
    /// `"readiness": null` erases the node-level one.
    #[serde(
        default,
        deserialize_with = "explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub readiness: Option<Option<HealthCheck>>,

    /// Liveness probe — runs continuously after the node is healthy.
    /// Triggers recovery when `failure_threshold` consecutive checks fail.
    /// Replaced wholesale; `"liveness": null` erases the node-level one.
    #[serde(
        default,
        deserialize_with = "explicit_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub liveness: Option<Option<LivenessProbe>>,
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

    /// The command run for `type: "command"`: `argv` or `shell` (or the v1/v2
    /// `command`).
    #[serde(flatten)]
    pub cmd: CommandKeys,

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

// ---------------------------------------------------------------------------
// Resolved variant — the single owner of the node → variant cascade
// ---------------------------------------------------------------------------

/// Everything a node+variant effectively is, after the node-level defaults are
/// applied.
///
/// **Every consumer must go through this.** F3, F6, and F7 all change how a
/// variant's effective config is computed, and a second resolution path in the
/// orchestrator would be invisible until it produced a wrong value at runtime
/// (RC5). So the orchestrator, the graph, the daemon monitor and the share flow
/// read `ResolvedVariant`, not `VariantConfig`.
///
/// The three merge strategies below are deliberately distinct and are not
/// unified — see the merge table in `docs/configuration.md`:
/// - **per key** (`env`, `ports`, `depends_on`) — additive, variant wins, `null`
///   erases.
/// - **per field** (`features`) — variant wins field by field.
/// - **wholesale replace** (`probes` per probe, `share`, `outputs`) — a variant
///   that states one replaces the node's entirely.
/// - `proxy` keeps its pre-existing union-with-cross-key-rule, untouched.
#[derive(Debug, Clone)]
pub struct ResolvedVariant {
    pub step_type: StepType,
    pub command: Option<CommandSpec>,
    pub script: Option<String>,
    pub readiness: Option<HealthCheck>,
    pub liveness: Option<LivenessProbe>,
    pub depends_on: Option<HashMap<String, String>>,
    pub env: Option<HashMap<String, ConfigValue>>,
    pub ports: Option<ResolvedPorts>,
    /// Paths (project-root-relative) to write before the process starts.
    pub files: Option<HashMap<String, FileDelivery>>,
    pub outputs: Option<Outputs>,
    pub sensitive_outputs: Option<Vec<String>>,
    pub strict_outputs: bool,
    pub skip_if: Option<CommandSpec>,
    pub on_stop: Option<CommandSpec>,
    pub share: Option<SharePolicy>,
    pub features: ResolvedFeatures,
    pub proxy: ResolvedProxy,
    pub client_log_levels: Vec<String>,
}

/// Resolve one node+variant against its node and the project.
///
/// `step_type` has no sensible default, so a variant that states none and whose
/// node states none falls back to `command` — the safer of the two, since a
/// mislabelled `start_server` would hang the graph waiting for readiness. The
/// `missing-step-type` validation rule reports it either way, so the fallback
/// only decides what happens while the author still has an invalid config.
pub fn resolve_variant(
    project: &VeldConfig,
    node: &NodeConfig,
    variant: &VariantConfig,
) -> ResolvedVariant {
    // Wholesale replace: a variant that states the field at all replaces the
    // node's value, and an explicit `null` erases it. `Some(None)` is the erase.
    fn replace<T: Clone>(node: Option<&T>, variant: Option<&Option<T>>) -> Option<T> {
        match variant {
            Some(Some(v)) => Some(v.clone()),
            Some(None) => None,
            None => node.cloned(),
        }
    }

    /// What one level (node or variant) says about its readiness probe.
    ///
    /// `None` = the level is silent, so the next level up applies.
    /// `Some(None)` = the level explicitly erased it.
    /// `Some(Some(p))` = the level sets it.
    ///
    /// `probes.readiness` supersedes the legacy `health_check` *within* a level,
    /// so a variant's `probes.readiness` still beats a variant's `health_check`,
    /// and either of them beats anything the node said.
    fn level_readiness(
        probes: Option<&ProbesConfig>,
        legacy: Option<&HealthCheck>,
    ) -> Option<Option<HealthCheck>> {
        if let Some(stated) = probes.and_then(|p| p.readiness.as_ref()) {
            return Some(stated.clone());
        }
        legacy.cloned().map(Some)
    }

    // A node has no legacy `health_check` field — the legacy form was only ever
    // on a variant — so the node level passes `None` for it.
    let readiness = match level_readiness(variant.probes.as_ref(), variant.health_check.as_ref()) {
        Some(from_variant) => from_variant,
        None => level_readiness(node.probes.as_ref(), None).flatten(),
    };

    let liveness = replace(
        node.probes
            .as_ref()
            .and_then(|p| p.liveness.as_ref())
            .and_then(|l| l.as_ref()),
        variant.probes.as_ref().and_then(|p| p.liveness.as_ref()),
    );

    ResolvedVariant {
        step_type: variant
            .step_type
            .or(node.step_type)
            .unwrap_or(StepType::Command),
        command: variant.cmd.spec().or_else(|| node.cmd.spec()),
        script: variant.script.clone(),
        readiness,
        liveness,
        depends_on: resolve_depends_on(node.depends_on.as_ref(), variant.depends_on.as_ref()),
        env: resolve_env(
            project.env.as_ref(),
            node.env.as_ref(),
            variant.env.as_ref(),
        ),
        ports: resolve_ports(node.ports.as_ref(), variant.ports.as_ref()),
        files: merge_nullable_maps([None, node.files.as_ref(), variant.files.as_ref()]),
        outputs: replace(node.outputs.as_ref(), variant.outputs.as_ref()),
        sensitive_outputs: variant.sensitive_outputs.clone(),
        strict_outputs: variant.strict_outputs,
        skip_if: variant.skip_if.clone(),
        // Absent inherits the node's hook; an explicit `null` erases it. `replace`
        // is the same three-way rule already used for `outputs` and `share`.
        on_stop: replace(node.on_stop.as_ref(), variant.on_stop.as_ref()),
        share: replace(node.share.as_ref(), variant.share.as_ref()),
        features: resolve_features(
            project.features.as_ref(),
            node.features.as_ref(),
            variant.features.as_ref(),
        ),
        proxy: resolve_proxy(
            project.proxy.as_ref(),
            node.proxy.as_ref(),
            variant.proxy.as_ref(),
        ),
        client_log_levels: resolve_client_log_levels(
            project.client_log_levels.as_deref(),
            node.client_log_levels.as_deref(),
            variant.client_log_levels.as_deref(),
        ),
    }
}

impl VeldConfig {
    /// Resolve a node+variant by name, if both exist.
    pub fn resolved(&self, node: &str, variant: &str) -> Option<ResolvedVariant> {
        let node_cfg = self.nodes.get(node)?;
        let variant_cfg = node_cfg.variants.get(variant)?;
        Some(resolve_variant(self, node_cfg, variant_cfg))
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

/// **Parse** a config file: structure only — file readable, JSON well-formed,
/// field types right, schema version supported.
///
/// This is the loader that runs on **every** subcommand (`stop`, `status`,
/// `logs`, …) and inside the daemon monitor, so it must succeed for any document
/// veld can still interpret. Never add a semantic check here: `on_stop` is read
/// from the on-disk config at stop time, so a config that fails to load means
/// teardown commands never run and containers leak with no way to clean up.
/// Semantic checks belong in [`validate`], which only `veld start`, `veld lint`,
/// and the share flow call.
pub fn parse_config(path: &Path) -> Result<VeldConfig, ConfigError> {
    parse_config_with_files(path).map(|loaded| loaded.config)
}

/// [`parse_config`], but keeping the per-file provenance: which files were loaded,
/// which glob matched each, which nodes each defines, and the hash over all of
/// them.
///
/// Callers that only need the effective config use [`parse_config`]. `veld nodes`,
/// `veld config --files`, and the run snapshot need the provenance — under
/// `include` globs, "unknown node" has four different causes (never defined,
/// defined but not matched by a glob, file renamed out of a glob, file present but
/// unparseable) and a bare name cannot tell them apart.
pub fn parse_config_with_files(path: &Path) -> Result<crate::include::LoadedConfig, ConfigError> {
    let mut loaded = crate::include::load(path)?;

    // Deferred (non-fatal) findings are collected per file — see
    // `VeldConfig::deferred_findings` for why a duplicate key must not fail the
    // load.
    for file in &loaded.files {
        if let Ok(text) = std::fs::read_to_string(&file.path) {
            if let Ok(json) = crate::jsonc::strip(&text) {
                if let Err(e) = crate::jsonc::reject_duplicate_keys(&json) {
                    loaded.config.deferred_findings.push(Finding::error(
                        "duplicate-key",
                        file.relative.display().to_string(),
                        format!(
                            "{e}. serde_json keeps the last value, so one of the two is \
                             being silently ignored"
                        ),
                    ));
                }
            }
        }
    }
    Ok(loaded)
}

/// Schema versions this build can load.
///
/// **Only `"3"`.** v1 and v2 are not supported: such a config must be rewritten,
/// and [`UnsupportedSchemaVersion`](ConfigError::UnsupportedSchemaVersion) states
/// every rule for doing so. Keeping two readings alive was tried and abandoned —
/// every rule then needed a severity that depended on the document's version,
/// every new field was silently live in an old document, and the result was two
/// config languages sharing one parser. One reading is the feature.
///
/// There is deliberately no automated converter. veld shipped one and removed it:
/// preserving comments meant rewriting bytes, and a byte-level rewriter cannot see
/// that `hooks` and `ui` are opaque, so it edited the blobs veld promises not to
/// interpret. Detection is structural and exact and stays here; the rewrite is a
/// judgment (`argv` or `shell`?) best left to whoever — or whatever — is reading
/// the config, with `veld lint` as the check afterwards.
pub const SUPPORTED_SCHEMA_VERSIONS: &[&str] = &["3"];

/// Every recognised top-level config key, for the unknown-key diagnostic.
///
/// Kept in sync with [`crate::include::Document`]'s fields by
/// `known_top_level_keys_matches_document`, because a list that drifts turns a
/// helpful error into a misleading one.
pub const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "$schema",
    "schemaVersion",
    "name",
    "include",
    "url_template",
    "presets",
    "default_preset",
    "client_log_levels",
    "features",
    "proxy",
    "env",
    "vars",
    "sharing",
    "setup",
    "teardown",
    "nodes",
    "hooks",
    "ui",
];

/// `command` is gone: every place that runs something says `argv` or `shell`.
///
/// This is a **structural** rule, not a semantic one — it is about which keys a
/// config may use at all, in the same class as a wrong field type — so it belongs
/// in the loader. That does mean such a document fails `veld stop` too; the
/// mitigation is that it has never run, because it cannot start either. (A config
/// that ran under an older veld declared `schemaVersion` 1 or 2, and now fails
/// earlier, at the version check, which explains the upgrade in full.)
///
/// Walking the raw value rather than the typed model catches every position at
/// once (variants, probes, actions, setup/teardown steps, value sources) without
/// enumerating them, and cannot drift as positions are added: no v3 schema
/// position uses the key at all.
pub(crate) fn reject_v3_legacy_commands(
    value: &serde_json::Value,
    path: &Path,
) -> Result<(), ConfigError> {
    fn walk(v: &serde_json::Value, at: &str, found: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    let here = if at.is_empty() {
                        key.clone()
                    } else {
                        format!("{at}.{key}")
                    };
                    // `hooks` and `ui` are reserved and **opaque** (F8): veld does
                    // not interpret their contents, so it must not police their key
                    // names either. A UI extension declaring a `command` key is its
                    // own business — treating it as veld's legacy key made the whole
                    // config unloadable. This exemption is also why the automated
                    // converter was removed: it worked on bytes, so it could not see
                    // the exemption and rewrote the opaque blobs anyway.
                    if at.is_empty() && (key == "hooks" || key == "ui") {
                        continue;
                    }
                    if key == "command" {
                        found.push(here.clone());
                    }
                    // `on_stop` / `skip_if` took a bare shell string in v1/v2;
                    // in v3 they carry an { argv | shell } object like everything
                    // else. `verify` is `skip_if`'s serde alias, so it has to be
                    // listed here too — the gate matches on the spelling in the
                    // file, and a key that deserializes into a gated field but is
                    // not itself gated is a hole with no signal.
                    if matches!(key.as_str(), "on_stop" | "skip_if" | "verify") && child.is_string()
                    {
                        found.push(here.clone());
                    }
                    walk(child, &here, found);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    walk(child, &format!("{at}[{i}]"), found);
                }
            }
            _ => {}
        }
    }

    let mut found = Vec::new();
    walk(value, "", &mut found);
    if found.is_empty() {
        return Ok(());
    }
    found.sort();
    Err(ConfigError::LegacyCommandInV3 {
        path: path.to_path_buf(),
        locations: found,
    })
}

/// **Validate** an already-parsed config: every semantic rule, reported as
/// [`Finding`]s rather than a single error so `veld lint` can show all of them
/// at once.
///
/// Deliberately separate from [`parse_config`] and returning a *different* type
/// than [`ConfigError`], so no diagnostic added here can ever reach a
/// subcommand that must keep working against a broken config. Callers:
/// `veld start` (refuses on any [`Severity::Error`]), `veld lint` (prints
/// everything), and the share flow (which emits proxy headers over the wire).
#[must_use = "validate reports problems; ignoring the findings defeats the point"]
pub fn validate(config: &VeldConfig) -> Vec<Finding> {
    // Problems the parser saw in the raw text but deliberately did not fail on.
    let mut findings = config.deferred_findings.clone();
    check_proxy_headers(config, &mut findings);
    check_depends_on_literal(config, &mut findings);
    check_exactly_one_command(config, &mut findings);
    check_builtin_names(config, &mut findings);
    check_resolved_variants(config, &mut findings);
    check_secret_usage(config, &mut findings);
    check_vars(config, &mut findings);
    check_presets(config, &mut findings);
    check_preset_keys(config, &mut findings);
    check_reserved_namespaces(config, &mut findings);
    // Total ordering, including `message`: several findings can share a
    // `location` (two bad `depends_on` entries in one variant), and `depends_on`
    // is a `HashMap`, so a partial sort would leave `veld lint --json` output
    // order varying run to run and any golden-file CI diff flapping.
    findings.sort_by(|a, b| {
        (a.severity, &a.location, &a.rule, &a.message).cmp(&(
            b.severity,
            &b.location,
            &b.rule,
            &b.message,
        ))
    });
    findings
}

/// F8: `hooks` and `ui` are reserved — parsed, stored, and **not executed** by
/// this version.
///
/// Saying so is the whole point of reserving them. An author who writes a
/// `worktree.created` hook and sees nothing happen has no way to tell a
/// not-yet-implemented feature from a config mistake, and would reasonably spend
/// an afternoon on the difference.
fn check_reserved_namespaces(config: &VeldConfig, out: &mut Vec<Finding>) {
    if let Some(hooks) = &config.hooks {
        let count = hooks.as_object().map(|o| o.len()).unwrap_or(0);
        out.push(Finding::notice(
            "reserved-not-executed",
            "hooks",
            format!(
                "`hooks` is declared ({count} event(s)); this version of veld parses and \
                 stores them but does not run them. The key is reserved so the shape does \
                 not change when it is implemented"
            ),
        ));
    }
    if let Some(ui) = &config.ui {
        let count = ui.as_object().map(|o| o.len()).unwrap_or(0);
        out.push(Finding::notice(
            "reserved-not-executed",
            "ui",
            format!(
                "`ui` is declared ({count} extension(s)); this version of veld parses and \
                 stores them but does not render them"
            ),
        ));
    }
}

/// The four `vars` rules (F4). They are the whole design, so all of them are
/// enforced rather than documented and hoped for.
///
/// Rule 1 — *a var is a scalar or a single value source, never an object* — is
/// enforced by the type: `vars` is a map of [`ConfigValue`], which cannot hold a
/// probe block or an `env` map. There is nothing to check here for it.
///
/// Rule 3 — *a duplicate var name is a hard error* — is enforced by the
/// duplicate-key check, since two entries of the same name in one `vars` object
/// is exactly that. Across files it becomes F2's problem.
/// Presets resolve: every `@ref` names a real preset, no cycles, and every
/// selection names a real node and variant.
///
/// None of this was checked. `expand_preset` catches an unknown reference and a
/// cycle, but only when `veld start` runs it, and the node/variant existence check
/// happened later still, during graph construction. So a config with `["@nope"]`,
/// with `a → b → a`, or naming a node that does not exist reported *"is valid"* —
/// which is the one thing `veld lint` exists not to do. F0.1's promise is that
/// every semantic problem is reported at once, before anything starts; a preset is
/// exactly the kind of thing edited by hand and used weeks later.
///
/// Reuses [`crate::graph::expand_preset`] rather than re-walking the references,
/// so the rule cannot disagree with the code that actually starts the run.
fn check_presets(config: &VeldConfig, out: &mut Vec<Finding>) {
    let Some(presets) = config.presets.as_ref() else {
        return;
    };
    let mut names: Vec<&String> = presets.keys().collect();
    names.sort();
    for name in names {
        match crate::graph::expand_preset(name, config) {
            Err(e) => out.push(Finding::error(
                "preset-unresolvable",
                format!("presets.{name}"),
                format!(
                    "{e}. `veld start --preset {name}` would fail here, so it is \
                     reported now rather than at start"
                ),
            )),
            Ok(selections) => {
                // `expand_preset` resolves references; it does not know whether the
                // node it named exists, which used to surface only once the graph
                // was built.
                for sel in selections {
                    let Some(node) = config.nodes.get(&sel.node) else {
                        out.push(Finding::error(
                            "preset-unknown-node",
                            format!("presets.{name}"),
                            format!(
                                "selects `{}:{}`, but no node named `{}` is defined. \
                                 With `include` globs a node can also be missing \
                                 because no glob matched its file — `veld config \
                                 --files` prints the glob → file → node chain",
                                sel.node, sel.variant, sel.node
                            ),
                        ));
                        continue;
                    };
                    if !node.variants.contains_key(&sel.variant) {
                        let mut known: Vec<&str> =
                            node.variants.keys().map(String::as_str).collect();
                        known.sort_unstable();
                        out.push(Finding::error(
                            "preset-unknown-variant",
                            format!("presets.{name}"),
                            format!(
                                "selects `{}:{}`, but node `{}` has no variant `{}` \
                                 (it has: {})",
                                sel.node,
                                sel.variant,
                                sel.node,
                                sel.variant,
                                known.join(", ")
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// The preset *number* rules — everything that can make the digit a person types
/// mean the wrong thing.
///
/// A pinned `key` is a promise: it is what somebody memorised, wrote in a
/// runbook, or said out loud to a colleague. So every way that promise can be
/// broken or ambiguous is an error here rather than a surprise at the picker,
/// where the only symptom is the wrong environment starting.
fn check_preset_keys(config: &VeldConfig, out: &mut Vec<Finding>) {
    let Some(presets) = config.presets.as_ref() else {
        // `default_preset` without any presets at all is still worth naming.
        check_default_preset(config, out);
        return;
    };

    // Duplicate pinned keys. Reported against every name involved, sorted, so
    // the message is the same whichever file the reader opens first.
    let mut by_key: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
    for (name, def) in presets {
        if let Some(key) = def.key() {
            by_key.entry(key).or_default().push(name.as_str());
        }
        if def.key() == Some(0) {
            out.push(Finding::error(
                "preset-invalid-key",
                format!("presets.{name}"),
                "\"key\": 0 is not selectable — the picker numbers from 1. Pin a key of 1 \
                 or more",
            ));
        }
    }
    for (key, mut names) in by_key {
        if names.len() > 1 {
            names.sort_unstable();
            for name in &names {
                out.push(Finding::error(
                    "preset-duplicate-key",
                    format!("presets.{name}"),
                    format!(
                        "pins \"key\": {key}, and so does {}. A key is the number a person \
                         types at the picker, so two presets cannot share one — give each \
                         its own",
                        names
                            .iter()
                            .filter(|n| *n != name)
                            .map(|n| format!("`{n}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
        }
    }

    // A preset *named* like a number that some other preset holds as a key.
    // `presets::find` resolves the key first, so the name becomes unreachable;
    // rather than document that as a rule, report it.
    let assigned: Vec<(String, u32)> = crate::presets::resolve(config)
        .into_iter()
        .map(|p| (p.name, p.key))
        .collect();
    for (name, own_key) in &assigned {
        if let Ok(as_number) = name.parse::<u32>()
            && let Some((owner, _)) = assigned
                .iter()
                .find(|(other, key)| *key == as_number && other != name)
        {
            out.push(Finding::warning(
                "preset-name-shadowed-by-key",
                format!("presets.{name}"),
                format!(
                    "is named `{name}`, which is also the key of preset `{owner}`. Typing \
                     `{name}` at the picker selects `{owner}`; select this one by its own \
                     key `{own_key}` instead. Renaming it also works, but breaks \
                     `--preset {name}` wherever that is already used"
                ),
            ));
        }
    }

    check_default_preset(config, out);

    // One notice, not one per preset: the array form is fully supported and a
    // small config saying `"dev": ["web:dev"]` needs no prose. It is at the scale
    // where the list stops fitting in a glance that unlabelled presets start
    // costing someone — a designer picking blind, or an agent guessing — which is
    // the only case worth interrupting anyone about.
    const CROWDED: usize = 8;
    let documented = presets
        .values()
        .any(|d| d.label().is_some() || d.when_to_use().is_some());
    if presets.len() >= CROWDED && !documented {
        out.push(Finding::notice(
            "presets-undocumented",
            "presets",
            format!(
                "{} presets, none with a \"label\" or \"when_to_use\". At this size the \
                 list is hard to pick from — for a person who did not write it, and for a \
                 coding agent reading `veld presets --json` to decide what to start. The \
                 object form adds both: `\"{}\": {{ \"label\": …, \"when_to_use\": …, \
                 \"selections\": [...] }}`",
                presets.len(),
                presets.keys().next().map_or("dev", String::as_str),
            ),
        ));
    }
}

/// `default_preset` must name a preset that exists — it is read when the user
/// gave veld nothing to start, which is the moment they are least equipped to
/// debug it.
fn check_default_preset(config: &VeldConfig, out: &mut Vec<Finding>) {
    let Some(name) = config.default_preset.as_deref() else {
        return;
    };
    let known: Vec<&str> = config
        .presets
        .iter()
        .flatten()
        .map(|(n, _)| n.as_str())
        .collect();
    if known.contains(&name) {
        return;
    }
    let mut sorted = known;
    sorted.sort_unstable();
    let known_list = if sorted.is_empty() {
        "no presets are defined".to_owned()
    } else {
        format!("known presets: {}", sorted.join(", "))
    };
    out.push(Finding::error(
        "default-preset-unknown",
        "default_preset",
        format!("names `{name}`, but {known_list}"),
    ));
}

fn check_vars(config: &VeldConfig, out: &mut Vec<Finding>) {
    let declared: BTreeMap<&str, &ConfigValue> = config
        .vars
        .iter()
        .flatten()
        .map(|(k, v)| (k.as_str(), v))
        .collect();

    // Rule 2: a var may not reference another var. One hop, always — provenance
    // stays a single lookup, and there is no ordering or cycle to reason about.
    for (name, value) in &declared {
        if let Some(literal) = value.as_literal() {
            for referenced in builtin_refs_in(literal, "vars.") {
                out.push(Finding::error(
                    "vars-cannot-nest",
                    format!("vars.{name}"),
                    format!(
                        "references `${{vars.{referenced}}}`. A var may not reference another \
                         var — one hop, always, so the value has exactly one definition \
                         point. Inline the value, or reference both vars at the use site"
                    ),
                ));
            }
        }
    }

    // Rule 4: an unknown `${vars.x}` is a hard error listing the declared names,
    // because the overwhelmingly likely cause is a typo and the fix is to see the
    // real list.
    let names: Vec<&str> = declared.keys().copied().collect();
    let report = |loc: String, referenced: &str, out: &mut Vec<Finding>| {
        if declared.contains_key(referenced) {
            return;
        }
        out.push(Finding::error(
            "unknown-var",
            loc,
            if names.is_empty() {
                format!("`${{vars.{referenced}}}` is referenced but no `vars` are declared")
            } else {
                format!(
                    "no var named \"{referenced}\" is declared. Declared vars: {}",
                    names.join(", ")
                )
            },
        ));
    };

    let check_str = |loc: String, s: &str, out: &mut Vec<Finding>| {
        for referenced in builtin_refs_in(s, "vars.") {
            report(loc.clone(), &referenced, out);
        }
    };

    for (key, value) in config.env.iter().flatten() {
        if let Some(literal) = value.as_ref().and_then(|v| v.as_literal()) {
            check_str(format!("env.{key}"), literal, out);
        }
    }
    for (node_name, node) in &config.nodes {
        for (key, value) in node.env.iter().flatten() {
            if let Some(literal) = value.as_ref().and_then(|v| v.as_literal()) {
                check_str(format!("nodes.{node_name}.env.{key}"), literal, out);
            }
        }
        for (variant_name, variant) in &node.variants {
            let base = format!("nodes.{node_name}.variants.{variant_name}");
            for (key, value) in variant.env.iter().flatten() {
                if let Some(literal) = value.as_ref().and_then(|v| v.as_literal()) {
                    check_str(format!("{base}.env.{key}"), literal, out);
                }
            }
            let r = resolve_variant(config, node, variant);
            for (suffix, spec) in [
                ("", r.command.as_ref()),
                (".on_stop", r.on_stop.as_ref()),
                (".skip_if", r.skip_if.as_ref()),
            ] {
                let Some(spec) = spec else { continue };
                let parts = match spec {
                    CommandSpec::Argv(a) => a.clone(),
                    CommandSpec::Shell(sh) => vec![sh.clone()],
                };
                for part in &parts {
                    check_str(format!("{base}{suffix}"), part, out);
                }
            }
        }
    }
}

/// Does this literal *look* like a credential that was pasted into the config?
///
/// Shape-based, not entropy-based, and deliberately conservative: a false
/// positive on a value the author knows is fine is a warning they can ignore,
/// whereas a false negative is a leaked token in version control. Recognises the
/// prefixed forms real providers use, a JWT, and a URL with inline credentials.
/// [`looks_like_a_credential`], applied to the value **and to each whitespace-
/// separated token in it**.
///
/// For a header value the token form is the normal one: `Authorization` is
/// `Bearer <token>` or `Basic <base64>`, so a whole-string check — which is what
/// the plain detector does, by design, since an `env` value is the credential
/// itself — sees `Bearer ghp_…`, matches nothing, and reports clean. That made the
/// first version of the proxy lint silently useless on the single case it exists
/// for. Kept separate rather than widening the plain detector, because broadening
/// the `env` rule to any token in a sentence would invent false positives in a rule
/// that already ships.
fn looks_like_a_credential_anywhere(value: &str) -> bool {
    looks_like_a_credential(value) || value.split_whitespace().any(looks_like_a_credential)
}

fn looks_like_a_credential(value: &str) -> bool {
    let v = value.trim();
    // Provider-prefixed tokens. These are *shapes*, not a vendor table — veld
    // knows nothing about the services, only that a string starting like this and
    // long enough to be real is almost never a deliberate literal.
    const PREFIXES: &[&str] = &[
        "sk-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "AKIA",
        "ASIA",
        "AIza",
        "glpat-",
        "dop_v1_",
        "shpat_",
        "npm_",
        "pypi-",
        "rk_live_",
        "sk_live_",
        "pk_live_",
    ];
    if PREFIXES
        .iter()
        .any(|p| v.starts_with(p) && v.len() >= p.len() + 12)
    {
        return true;
    }
    // A JWT: three base64url segments separated by dots, starting with a header.
    if v.starts_with("eyJ") && v.matches('.').count() == 2 {
        return true;
    }
    // `scheme://user:pass@host` — the classic accidental commit.
    if let Some((scheme, rest)) = v.split_once("://") {
        if !scheme.is_empty() && !scheme.contains(char::is_whitespace) {
            if let Some((authority, _)) = rest.split_once('/').or(Some((rest, ""))) {
                if let Some((userinfo, _)) = authority.split_once('@') {
                    if let Some((_, password)) = userinfo.split_once(':') {
                        // An empty or placeholder-length password is not worth a
                        // warning; `postgres://user:postgres@localhost` in a local
                        // dev config is the legitimate fixed-credential case.
                        return password.len() >= 12;
                    }
                }
            }
        }
    }
    false
}

/// F7's sensitivity rules.
///
/// The point of `secret: true` is that veld can then *refuse* the unsafe uses. A
/// secret's only sanctioned destinations are a child process's environment and a
/// file — never an `argv` element, never a `shell` string, never a log, never
/// `--json` output, never the share payload.
fn check_secret_usage(config: &VeldConfig, out: &mut Vec<Finding>) {
    /// Every name that, if interpolated into a command, would put a secret in the
    /// process table — paired with the reference forms that would do it.
    ///
    /// Three sources, because a secret can arrive by three routes and missing any
    /// one of them makes `secret: true` a false promise:
    /// - a `secret` **env** key, reachable as `${ENV_KEY}` in a shell command;
    /// - a `secret` **var**, reachable as `${vars.NAME}` anywhere;
    /// - a **sensitive output**, reachable as `${output.KEY}` and
    ///   `${nodes.<node>.KEY}` — `on_stop` and `actions` genuinely have these in
    ///   scope.
    fn secret_refs(config: &VeldConfig, node: &NodeConfig, v: &VariantConfig) -> Vec<String> {
        let mut refs: Vec<String> = Vec::new();

        for (key, value) in resolve_env(config.env.as_ref(), node.env.as_ref(), v.env.as_ref())
            .into_iter()
            .flatten()
            .filter(|(_, value)| value.secret)
        {
            let _ = value;
            // A shell command reaches an env value as `$KEY` / `${KEY}`.
            refs.push(key);
        }

        for name in config
            .vars
            .iter()
            .flatten()
            .filter(|(_, value)| value.secret)
            .map(|(name, _)| name)
        {
            // `${vars.NAME}` is expanded by veld itself, in every position.
            refs.push(format!("vars.{name}"));
        }

        // `sensitive_outputs` predates the `secret` flag and means the same thing
        // for this rule's purposes.
        let resolved = resolve_variant(config, node, v);
        for key in resolved.sensitive_outputs.iter().flatten() {
            refs.push(format!("output.{key}"));
            refs.push(key.clone());
        }

        refs
    }

    // A credential-shaped `proxy` header value.
    //
    // `proxy.*.set` values are plain strings by design — they are also the wire
    // payload sent to Caddy and to the public gateway, so they cannot carry a
    // `secret` flag or a value source. That makes shape the only signal available,
    // and it is worth acting on: an `Authorization` header is one of the most
    // natural things to set here, and a resolved header value travels verbatim to
    // every joiner of a share and to the gateway. Scrubbing it from the manifest is
    // not an option — the remote proxy needs the value to apply the rule — so the
    // honest mitigation is to refuse the shape at authoring time.
    {
        let mut check_rules = |location: String, rules: Option<&HeaderRules>| {
            for (header, value) in rules.map(|r| &r.set).into_iter().flatten() {
                if looks_like_a_credential_anywhere(value) {
                    out.push(Finding::warning(
                        "credential-shaped-proxy-header",
                        format!("{location}.set.{header}"),
                        format!(
                            "the value set for `{header}` looks like a real credential. \
                             A proxy header value cannot be marked `secret` or read from \
                             a source — it is part of the route sent to Caddy and, when \
                             the node is shared, to the public gateway and every joiner, \
                             verbatim. Treat it as published: use a credential scoped to \
                             local development, and never a production one"
                        ),
                    ));
                }
            }
        };
        let mut check_proxy = |location: String, proxy: Option<&ProxyConfig>| {
            if let Some(p) = proxy {
                check_rules(format!("{location}.request"), p.request.as_ref());
                check_rules(format!("{location}.response"), p.response.as_ref());
            }
        };
        check_proxy("proxy".to_owned(), config.proxy.as_ref());
        for (node_name, node) in &config.nodes {
            check_proxy(format!("nodes.{node_name}.proxy"), node.proxy.as_ref());
            for (variant_name, variant) in &node.variants {
                check_proxy(
                    format!("nodes.{node_name}.variants.{variant_name}.proxy"),
                    variant.proxy.as_ref(),
                );
            }
        }
    }

    for (node_name, node) in &config.nodes {
        // A credential-shaped literal is worth flagging wherever it sits, marked
        // secret or not: marking it keeps it out of `argv`, but it is still in
        // version control.
        for (level, env) in [
            ("env".to_owned(), config.env.as_ref()),
            (format!("nodes.{node_name}.env"), node.env.as_ref()),
        ] {
            for (key, value) in env.into_iter().flatten() {
                if let Some(literal) = value.as_ref().and_then(|v| v.as_literal()) {
                    if looks_like_a_credential(literal) {
                        out.push(Finding::warning(
                            "credential-shaped-literal",
                            format!("{level}.{key}"),
                            "this value looks like a real credential written into the \
                             config, where it lands in version control. Use \
                             `{ \"env\": … }`, `{ \"file\": … }`, or `{ \"argv\": … }` to \
                             keep it out. (A deliberate fixed local credential is fine — \
                             mark it `{ \"value\": …, \"secret\": true }` and this stays \
                             quiet.)",
                        ));
                    }
                }
            }
        }

        for (variant_name, variant) in &node.variants {
            let base = format!("nodes.{node_name}.variants.{variant_name}");
            for (key, value) in variant.env.iter().flatten() {
                if let Some(literal) = value.as_ref().and_then(|v| v.as_literal()) {
                    if looks_like_a_credential(literal) {
                        out.push(Finding::warning(
                            "credential-shaped-literal",
                            format!("{base}.env.{key}"),
                            "this value looks like a real credential written into the \
                             config, where it lands in version control. Use \
                             `{ \"env\": … }`, `{ \"file\": … }`, or `{ \"argv\": … }` to \
                             keep it out.",
                        ));
                    }
                }
            }

            // A secret interpolated into a command is an error, not a warning: an
            // `argv` element and a `shell` string both end up in the process
            // table, where every other user on the machine can read them, and in
            // any shell history or CI log that echoes the command.
            // No `secrets.is_empty()` early exit: a `${nodes.<other>.KEY}` leak is
            // declared by the *producing* node, so a variant with no secrets of its
            // own can still leak one. Skipping here meant the consuming side — the
            // only side that puts the value on a command line — was never scanned.
            let secrets = secret_refs(config, node, variant);
            let r = resolve_variant(config, node, variant);
            // EVERY command position, not just the variant's own. Actions are where
            // `${output.*}` is actually in scope, and a probe command runs on the
            // same machine with the same visibility — scanning only the variant left
            // the reachable positions unchecked.
            let mut commands: Vec<(String, CommandSpec)> = Vec::new();
            if let Some(c) = r.command.clone() {
                commands.push((String::new(), c));
            }
            if let Some(c) = r.on_stop.clone() {
                commands.push((".on_stop".to_owned(), c));
            }
            if let Some(c) = r.skip_if.clone() {
                commands.push((".skip_if".to_owned(), c));
            }
            if let Some(c) = r.readiness.as_ref().and_then(|p| p.cmd.spec()) {
                commands.push((".probes.readiness".to_owned(), c));
            }
            if let Some(c) = r.liveness.as_ref().and_then(|p| p.cmd.spec()) {
                commands.push((".probes.liveness".to_owned(), c));
            }
            for action in node.actions.iter().flatten() {
                if let Some(c) = action.cmd.spec() {
                    commands.push((format!(" (action {})", action.name), c));
                }
            }
            for (suffix, spec) in &commands {
                let parts: Vec<String> = match spec {
                    CommandSpec::Argv(a) => a.clone(),
                    CommandSpec::Shell(sh) => vec![sh.clone()],
                };
                for part in &parts {
                    // Every reference form a secret can arrive through.
                    let mut names = builtin_refs_in(part, "output.")
                        .into_iter()
                        .map(|n| format!("output.{n}"))
                        .collect::<Vec<_>>();
                    names.extend(
                        builtin_refs_in(part, "vars.")
                            .into_iter()
                            .map(|n| format!("vars.{n}")),
                    );
                    // `${nodes.<other>.KEY}` is resolved against **that** node's
                    // sensitivity, not this one's. Taking the trailing field and
                    // matching it here made a name sensitive project-wide the moment
                    // any single node declared it: a config reading
                    // `${nodes.postgres.DATABASE_URL}` was rejected because some
                    // *other* node happened to call one of its own outputs
                    // `DATABASE_URL`, and the message claimed the value was declared
                    // secret when it never was. A false positive on a security rule
                    // is expensive — it teaches people to work around the linter.
                    for r in builtin_refs_in(part, "nodes.") {
                        let mut it = r.splitn(2, '.');
                        let (Some(other), Some(key)) = (it.next(), it.next()) else {
                            continue;
                        };
                        if other == node_name {
                            // Its own outputs are already covered by `output.KEY`.
                            continue;
                        }
                        if let Some(other_node) = config.nodes.get(other) {
                            // The producing variant is not known until start, so any
                            // variant declaring the key sensitive is enough. Erring
                            // toward flagging is the right direction here.
                            let sensitive = other_node.variants.values().any(|ov| {
                                resolve_variant(config, other_node, ov)
                                    .sensitive_outputs
                                    .iter()
                                    .flatten()
                                    .any(|k| k == key)
                            });
                            if sensitive {
                                names.push(format!("nodes.{other}.{key}"));
                            }
                        }
                    }
                    names.extend(env_refs(part));
                    for name in names {
                        // A `nodes.<other>.KEY` name is only ever pushed above once
                        // that node's own declaration has been checked, so it is
                        // already known sensitive and does not belong in `secrets`
                        // (which describes *this* variant).
                        if secrets.contains(&name) || name.starts_with("nodes.") {
                            out.push(Finding::error(
                                "secret-in-command",
                                format!("{base}{suffix}"),
                                {
                                    // The remedy differs by form, so name the right
                                    // one rather than telling a `vars` author about
                                    // an environment variable that does not exist.
                                    let remedy = if let Some(var) = name.strip_prefix("vars.") {
                                        format!(
                                            "put it in `env` as \
                                             `{{ \"SOME_NAME\": \"${{vars.{var}}}\" }}` and \
                                             have the program read SOME_NAME from its \
                                             environment, or deliver it with `files`"
                                        )
                                    } else if let Some(out) = name.strip_prefix("output.") {
                                        format!(
                                            "pass it through `env` (veld exports a node's \
                                             outputs to its own process) rather than \
                                             interpolating ${{output.{out}}} into the \
                                             command line"
                                        )
                                    } else if let Some(rest) = name.strip_prefix("nodes.") {
                                        // Another node's output: veld does not export it
                                        // here, so the author has to route it explicitly.
                                        format!(
                                            "put it in this variant's `env` as \
                                             `{{ \"SOME_NAME\": \"${{nodes.{rest}}}\" }}` and \
                                             have the program read SOME_NAME from its \
                                             environment, or deliver it with `files` — \
                                             veld does not export another node's outputs \
                                             into this process automatically"
                                        )
                                    } else {
                                        format!(
                                            "veld already passes it to the process as the \
                                             environment variable {name}, so have the program \
                                             read it from the environment instead of taking \
                                             it as an argument"
                                        )
                                    };
                                    format!(
                                        "{name} is declared secret, so it must not be \
                                         interpolated into a command — an argv element and a \
                                         shell string both appear in the process table, where \
                                         any user on the machine can read them, and in CI logs \
                                         that echo the command. Instead: {remedy}"
                                    )
                                },
                            ));
                        }
                    }
                }
            }
        }
    }
}

/// `${<prefix><name>}` references in `s`.
pub(crate) fn builtin_refs_in(s: &str, prefix: &str) -> Vec<String> {
    let needle = format!("${{{prefix}");
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find(&needle) {
        let after = &rest[start + needle.len()..];
        match after.find('}') {
            None => break,
            Some(end) => {
                out.push(after[..end].to_owned());
                rest = &after[end + 1..];
            }
        }
    }
    out
}

/// Bare `$NAME` / `${NAME}` shell references, which is how a `shell` command
/// would reach an env value directly.
pub(crate) fn env_refs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let braced = bytes.get(j) == Some(&b'{');
        if braced {
            j += 1;
        }
        let start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
            j += 1;
        }
        if j > start {
            out.push(s[start..j].to_owned());
        }
        i = if braced && bytes.get(j) == Some(&b'}') {
            j + 1
        } else {
            j.max(i + 1)
        };
    }
    out
}

/// Rules that only make sense on the **resolved** variant — after node-level
/// defaults are applied — so a field hoisted to node level is judged where it
/// actually takes effect rather than where it happens to be written.
fn check_resolved_variants(config: &VeldConfig, out: &mut Vec<Finding>) {
    for (node_name, node) in &config.nodes {
        for (variant_name, variant) in &node.variants {
            let loc = format!("nodes.{node_name}.variants.{variant_name}");
            let r = resolve_variant(config, node, variant);

            // `type` has no default worth guessing at. `resolve_variant` falls
            // back to `command` so a broken config still behaves predictably, but
            // the author has to be told.
            if variant.step_type.is_none() && node.step_type.is_none() {
                out.push(Finding::error(
                    "missing-step-type",
                    &loc,
                    "no \"type\" — set it on the variant, or once on the node for all \
                     its variants. Expected \"start_server\" or \"command\"",
                ));
            }

            // Which port `${veld.port}` means must never be a guess.
            if let Some(ports) = &r.ports {
                if ports.primary.is_empty() {
                    let mut names: Vec<&str> = ports.ports.keys().map(String::as_str).collect();
                    names.sort();
                    out.push(Finding::error(
                        "ambiguous-primary-port",
                        format!("{loc}.ports"),
                        format!(
                            "several ports are declared ({}) and none is named \"{}\", so \
                             `${{veld.port}}` has no unambiguous meaning. Name the main one \
                             \"{}\"",
                            names.join(", "),
                            PRIMARY_PORT_NAME,
                            PRIMARY_PORT_NAME
                        ),
                    ));
                }
            }

            // A `start_server` with no readiness probe reports healthy the moment
            // its port opens, so the graph proceeds before the service can serve.
            //
            // Severity is version-gated on purpose. The issue specifies this as an
            // error, but applying it to a v1 or v2 document would break the
            // acceptance criterion that such a config "loads and runs
            // byte-identically, with no new warnings" — probeless `start_server`
            // nodes are common in configs written before probes existed, and
            // failing them at `veld start` would be a flag day. So: an error in a
            // v3 document (which is opting into the new rules), a warning in v1/v2
            // (surfaced by `veld lint`, never blocking a run).
            if r.step_type == StepType::StartServer && r.readiness.is_none() {
                out.push(Finding::error(
                    "start-server-needs-readiness",
                    &loc,
                    "a `start_server` with no readiness probe is reported healthy as soon as \
                     its port opens, so dependents start before it can serve. Add \
                     `probes.readiness` — `{ \"type\": \"http\", \"path\": \"/…\" }`, or \
                     `{ \"type\": \"port\" }` to accept the port-open check as readiness",
                ));
            }
        }
    }
}

/// The complete `veld.*` namespace — a **closed** set.
///
/// Node outputs are deliberately absent: they live in `${output.*}` (own node)
/// and `${nodes.<node>.<field>}` (any node). Injecting them here let an output
/// named `port`, `run`, or `branch` shadow the builtin on some paths and not
/// others, so the same string resolved to different values (F0.2).
///
/// Availability is not uniform, and [`check_builtin_names`] deliberately does not
/// model that — it only rejects names that are not builtins at all:
/// - `port` / `url` / `url.*` exist only for a `start_server` node.
/// - `node` / `variant` are absent in project-level `setup` / `teardown` steps.
///
/// One family is **not** listed here because it is per-node rather than fixed:
/// `ports.<name>`, one per entry in the node's `ports` map (F6).
/// [`check_builtin_names`] validates those against what the node actually
/// declares, which gives a better error than "unknown builtin" — it can say which
/// port names exist.
pub const BUILTIN_VARS: &[&str] = &[
    "run",
    "run_id",
    "root",
    "project",
    "name",
    "worktree",
    "branch",
    "username",
    "node",
    "variant",
    "port",
    "url",
    "url.hostname",
    "url.host",
    "url.origin",
    "url.scheme",
    "url.port",
];

/// Reject `${veld.<name>}` references to names that are not builtins.
///
/// This is the guard for the F0.2 namespace closure. Before it, writing
/// `${veld.exit_code}` in an `on_stop` hook — which veld's own docs used to
/// recommend — produced `VariableError::UnknownBuiltin` at *teardown* time, where
/// `run_on_stop_hook` could only log a warning and skip the hook: the container
/// never got removed and `veld stop` still reported success. Catching the name
/// here turns a silent teardown skip into a refusal at `veld start`, before
/// anything is running that would need tearing down.
fn check_builtin_names(config: &VeldConfig, out: &mut Vec<Finding>) {
    const RULE: &str = "unknown-builtin-var";

    /// `declared_ports` is the node's `ports` map, so `${veld.ports.debug}`
    /// resolves iff the node declares a port called `debug`.
    fn check(loc: &str, s: &str, declared_ports: &[String], out: &mut Vec<Finding>) {
        for name in builtin_refs(s) {
            if BUILTIN_VARS.contains(&name.as_str()) {
                continue;
            }
            // `ports.<name>` is per-node, so it is checked against the node.
            if let Some(port_name) = name.strip_prefix("ports.") {
                if declared_ports.iter().any(|p| p == port_name) {
                    continue;
                }
                out.push(Finding::error(
                    RULE,
                    loc,
                    if declared_ports.is_empty() {
                        format!(
                            "`${{veld.ports.{port_name}}}` refers to a named port, but this \
                             node declares no `ports` map. Add one — \
                             `\"ports\": {{ \"{port_name}\": \"auto\" }}` — or use \
                             `${{veld.port}}` for the single allocated port"
                        )
                    } else {
                        format!(
                            "this node declares no port named \"{port_name}\". \
                             Declared ports: {}",
                            declared_ports.join(", ")
                        )
                    },
                ));
                continue;
            }
            // Point at the namespace that almost certainly holds it: an author
            // writing `${veld.DB_HOST}` means this node's output.
            out.push(Finding::error(
                RULE,
                loc,
                format!(
                    "`${{veld.{name}}}` is not a built-in variable. `veld.*` is a closed set \
                     ({}). If {name} is a node output, use `${{output.{name}}}` (this node) \
                     or `${{nodes.<node>.{name}}}` (another node)",
                    BUILTIN_VARS.join(", ")
                ),
            ));
        }
    }

    fn check_cmd(
        loc: &str,
        spec: Option<CommandSpec>,
        declared_ports: &[String],
        out: &mut Vec<Finding>,
    ) {
        match spec {
            Some(CommandSpec::Argv(argv)) => {
                for a in &argv {
                    check(loc, a, declared_ports, out);
                }
            }
            Some(CommandSpec::Shell(s)) => check(loc, &s, declared_ports, out),
            None => {}
        }
    }

    for (key, value) in config.env.iter().flatten() {
        // A `null` value erases an inherited key, and only an inline literal is
        // interpolated — a fetched value is used verbatim.
        if let Some(literal) = value.as_ref().and_then(|v| v.as_literal()) {
            check(&format!("env.{key}"), literal, &[], out);
        }
    }
    for (node_name, node) in &config.nodes {
        // A node-level `env` value is inherited by every variant, so it may only
        // reference a port the node itself declares.
        let node_ports: Vec<String> = resolve_ports(node.ports.as_ref(), None)
            .map(|p| p.ports.keys().cloned().collect())
            .unwrap_or_default();
        for (key, value) in node.env.iter().flatten() {
            if let Some(literal) = value.as_ref().and_then(|v| v.as_literal()) {
                check(
                    &format!("nodes.{node_name}.env.{key}"),
                    literal,
                    &node_ports,
                    out,
                );
            }
        }
        for (variant_name, variant) in &node.variants {
            let base = format!("nodes.{node_name}.variants.{variant_name}");
            let ports: Vec<String> = resolve_ports(node.ports.as_ref(), variant.ports.as_ref())
                .map(|p| p.ports.keys().cloned().collect())
                .unwrap_or_default();
            check_cmd(&base, variant.cmd.spec(), &ports, out);
            check_cmd(
                &format!("{base}.on_stop"),
                // `Some(None)` is an explicit erase — nothing to check.
                variant.on_stop.clone().flatten(),
                &ports,
                out,
            );
            check_cmd(
                &format!("{base}.skip_if"),
                variant.skip_if.clone(),
                &ports,
                out,
            );
            for (key, value) in variant.env.iter().flatten() {
                if let Some(literal) = value.as_ref().and_then(|v| v.as_literal()) {
                    check(&format!("{base}.env.{key}"), literal, &ports, out);
                }
            }
        }
    }
}

/// Every `${veld.<name>}` reference in `s`, in order of appearance.
fn builtin_refs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("${veld.") {
        let after = &rest[start + "${veld.".len()..];
        match after.find('}') {
            // An unclosed `${` is a different problem; interpolation reports it.
            None => break,
            Some(end) => {
                out.push(after[..end].to_owned());
                rest = &after[end + 1..];
            }
        }
    }
    out
}

/// Every place that runs something must declare **exactly one** of `argv` /
/// `shell` (or, in a v1/v2 document, `command`).
///
/// Declaring two is ambiguous — `CommandSpec::from_keys` has to pick one, and
/// whichever it picks is a coin toss from the author's point of view. Declaring
/// none means a step that silently does nothing. Both are caught here rather
/// than at spawn time, so the author learns before the run starts.
fn check_exactly_one_command(config: &VeldConfig, out: &mut Vec<Finding>) {
    const RULE: &str = "exactly-one-command";

    fn check(loc: String, keys: &CommandKeys, required: bool, out: &mut Vec<Finding>) {
        match keys.count_set() {
            0 if required => out.push(Finding::error(
                RULE,
                loc,
                "declares no command — set exactly one of \"argv\" or \"shell\"",
            )),
            0 | 1 => {}
            _ => out.push(Finding::error(
                RULE,
                loc,
                format!(
                    "declares {} at once ({}) — set exactly one of \"argv\" or \"shell\"",
                    keys.count_set(),
                    keys.set_names().join(", ")
                ),
            )),
        }
    }

    for (i, step) in config.setup.iter().flatten().enumerate() {
        check(format!("setup[{i}] ({})", step.name), &step.cmd, true, out);
    }
    for (i, step) in config.teardown.iter().flatten().enumerate() {
        check(
            format!("teardown[{i}] ({})", step.name),
            &step.cmd,
            true,
            out,
        );
    }
    for (node_name, node) in &config.nodes {
        for action in node.actions.iter().flatten() {
            check(
                format!("nodes.{node_name}.actions.{}", action.name),
                &action.cmd,
                true,
                out,
            );
        }
        for (variant_name, variant) in &node.variants {
            let base = format!("nodes.{node_name}.variants.{variant_name}");
            // Required-ness comes from the **resolved** variant: `argv`/`shell` is
            // hoistable to node level, so a variant that states nothing is correct
            // as long as its node does. Judging the raw variant made node-level
            // hoisting fail its own linter.
            //
            // The >1 check stays per level, so node `shell` + variant `argv` remains
            // a legal override rather than a conflict.
            let resolved_has_command = resolve_variant(config, node, variant).command.is_some();
            check(
                base.clone(),
                &variant.cmd,
                !resolved_has_command && variant.script.is_none(),
                out,
            );
            if let Some(probes) = &variant.probes {
                if let Some(Some(readiness)) = &probes.readiness {
                    // Only a `command`-type probe runs anything; an http/port
                    // probe legitimately declares none.
                    let needs = matches!(readiness.check_type.as_str(), "command" | "bash");
                    check(
                        format!("{base}.probes.readiness"),
                        &readiness.cmd,
                        needs,
                        out,
                    );
                }
                if let Some(Some(liveness)) = &probes.liveness {
                    let needs = matches!(liveness.check_type.as_str(), "command" | "bash");
                    check(format!("{base}.probes.liveness"), &liveness.cmd, needs, out);
                }
            }
            if let Some(hc) = &variant.health_check {
                let needs = matches!(hc.check_type.as_str(), "command" | "bash");
                check(format!("{base}.health_check"), &hc.cmd, needs, out);
            }
        }
    }
}

/// `depends_on` node and variant names must be literal — no `${…}`.
///
/// The dependency graph is read in [`crate::graph`] *before* any
/// `VariableContext` exists (and `${veld.port}` is only set after port
/// allocation, which itself runs after `build_execution_plan`), so an
/// interpolated dependency key would need a two-stage evaluator. Reject it with
/// a clear message instead of resolving it to a node name that does not exist.
fn check_depends_on_literal(config: &VeldConfig, out: &mut Vec<Finding>) {
    const RULE: &str = "depends-on-literal";
    for (node_name, node) in &config.nodes {
        for (variant_name, variant) in &node.variants {
            let Some(deps) = &variant.depends_on else {
                continue;
            };
            for (dep_node, dep_variant) in deps {
                // `null` erases an inherited dependency; there is no name to check.
                let Some(dep_variant) = dep_variant else {
                    continue;
                };
                let loc =
                    format!("nodes.{node_name}.variants.{variant_name}.depends_on.{dep_node}");
                if dep_node.contains("${") {
                    out.push(Finding::error(
                        RULE,
                        &loc,
                        format!(
                            "dependency name {dep_node:?} contains an interpolation — \
                             `depends_on` is read before any variable exists, so node and \
                             variant names must be written literally"
                        ),
                    ));
                }
                if dep_variant.contains("${") {
                    out.push(Finding::error(
                        RULE,
                        &loc,
                        format!(
                            "dependency variant {dep_variant:?} (of {dep_node:?}) contains an \
                             interpolation — `depends_on` is read before any variable exists, \
                             so node and variant names must be written literally"
                        ),
                    ));
                }
            }
        }
    }
}

/// Reject syntactically invalid proxy header names/values, once and loudly.
/// Both proxies otherwise skip invalid headers silently (the gateway) or hand
/// them to Caddy verbatim (the local proxy), so a typo like `"X Frame Options"`
/// would no-op with no diagnostic — and an unvalidated value reaching the
/// persisted Caddy route could poison the shared config reload.
fn check_proxy_headers(config: &VeldConfig, out: &mut Vec<Finding>) {
    const RULE: &str = "proxy-header-syntax";

    fn check_side(location: &str, side: &str, rules: &HeaderRules, out: &mut Vec<Finding>) {
        let loc = format!("{location}.{side}");
        for name in &rules.remove {
            if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
                out.push(Finding::error(
                    RULE,
                    &loc,
                    format!("invalid header name to remove: {name:?}"),
                ));
            }
        }
        for (name, value) in &rules.set {
            if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
                out.push(Finding::error(
                    RULE,
                    &loc,
                    format!("invalid header name to set: {name:?}"),
                ));
            }
            if reqwest::header::HeaderValue::from_str(value).is_err() {
                out.push(Finding::error(
                    RULE,
                    &loc,
                    format!("invalid value for header {name:?}: {value:?}"),
                ));
            }
        }
    }
    fn check(location: &str, proxy: &ProxyConfig, out: &mut Vec<Finding>) {
        if let Some(rules) = &proxy.request {
            check_side(location, "request", rules, out);
        }
        if let Some(rules) = &proxy.response {
            check_side(location, "response", rules, out);
        }
    }

    if let Some(proxy) = &config.proxy {
        check("proxy", proxy, out);
    }
    for (node_name, node) in &config.nodes {
        if let Some(proxy) = &node.proxy {
            check(&format!("nodes.{node_name}.proxy"), proxy, out);
        }
        for (variant_name, variant) in &node.variants {
            if let Some(proxy) = &variant.proxy {
                check(
                    &format!("nodes.{node_name}.variants.{variant_name}.proxy"),
                    proxy,
                    out,
                );
            }
        }
    }
}

/// Convenience: discover from CWD and parse (no semantic validation — see
/// [`parse_config`]).
pub fn parse_config_from_cwd() -> Result<(PathBuf, VeldConfig), ConfigError> {
    let cwd = std::env::current_dir().map_err(|e| ConfigError::ReadError {
        path: PathBuf::from("."),
        source: e,
    })?;
    let path = discover_config(&cwd)?;
    let config = parse_config(&path)?;
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

    fn rules(remove: &[&str], set: &[(&str, &str)]) -> HeaderRules {
        HeaderRules {
            remove: remove.iter().map(|s| s.to_string()).collect(),
            set: set
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn resolve_proxy_absent_is_empty() {
        assert!(resolve_proxy(None, None, None).is_empty());
    }

    #[test]
    fn resolve_proxy_unions_removes_and_merges_sets() {
        let project = ProxyConfig {
            request: Some(rules(&["Origin"], &[("X-A", "p"), ("X-B", "p")])),
            response: None,
        };
        let node = ProxyConfig {
            request: Some(rules(&["Referer"], &[("X-B", "n")])),
            response: Some(rules(&["Server"], &[])),
        };
        let variant = ProxyConfig {
            // Case-insensitive dedup: "origin" must not re-add "Origin".
            request: Some(rules(&["origin"], &[("X-C", "v")])),
            response: None,
        };
        let r = resolve_proxy(Some(&project), Some(&node), Some(&variant));
        // remove: union, first spelling wins, no case-dup.
        assert_eq!(r.request.remove, vec!["Origin", "Referer"]);
        // set: variant/node override project per key.
        assert_eq!(r.request.set.get("X-A").unwrap(), "p");
        assert_eq!(r.request.set.get("X-B").unwrap(), "n");
        assert_eq!(r.request.set.get("X-C").unwrap(), "v");
        // response only came from node.
        assert_eq!(r.response.remove, vec!["Server"]);
    }

    #[test]
    fn proxy_config_roundtrips_json() {
        let json = r#"{"request":{"remove":["Origin"],"set":{"X-Foo":"bar"}}}"#;
        let cfg: ProxyConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.request.as_ref().unwrap().remove, vec!["Origin"]);
        // Empty response is skipped on the way back out.
        let out = serde_json::to_string(&cfg).unwrap();
        assert!(!out.contains("response"), "empty response omitted: {out}");
    }

    #[test]
    fn proxy_config_rejects_misnested_fields_but_header_rules_are_lenient() {
        // ProxyConfig is deny_unknown_fields: the natural-but-wrong flattened
        // shape (missing request/response nesting) and pluralized keys are caught.
        assert!(serde_json::from_str::<ProxyConfig>(r#"{"remove":["Origin"]}"#).is_err());
        assert!(serde_json::from_str::<ProxyConfig>(r#"{"requests":{}}"#).is_err());
        // HeaderRules is intentionally lenient (it is also the wire type embedded
        // in the share manifest), so a key typo inside request/response is ignored
        // rather than failing the whole manifest on a version-skewed receiver —
        // the JSON schema catches it in-editor. It must NOT hard-fail here.
        let lenient: HeaderRules = serde_json::from_str(r#"{"remve":["Origin"]}"#).unwrap();
        assert!(lenient.is_empty());
    }

    #[test]
    fn resolve_proxy_set_override_is_case_insensitive() {
        let project = ProxyConfig {
            request: Some(rules(&[], &[("X-Frame-Options", "DENY")])),
            response: None,
        };
        let variant = ProxyConfig {
            request: Some(rules(&[], &[("x-frame-options", "SAMEORIGIN")])),
            response: None,
        };
        let r = resolve_proxy(Some(&project), None, Some(&variant));
        // Exactly one entry survives, the more-specific value wins.
        assert_eq!(r.request.set.len(), 1);
        let (_, v) = r.request.set.iter().next().unwrap();
        assert_eq!(v, "SAMEORIGIN");
    }

    #[test]
    fn resolve_proxy_set_wins_over_remove_for_same_header() {
        let cfg = ProxyConfig {
            request: Some(rules(&["X-Foo"], &[("x-foo", "bar")])),
            response: None,
        };
        let r = resolve_proxy(Some(&cfg), None, None);
        // The overlap is dropped from remove — set wins, identically on both proxies.
        assert!(r.request.remove.is_empty());
        assert_eq!(r.request.set.get("x-foo").unwrap(), "bar");
    }

    #[test]
    fn validate_rejects_bad_proxy_header_names_and_values() {
        let mut config: VeldConfig = serde_json::from_str(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"local":{"type":"start_server"}}}}}"#,
        )
        .unwrap();
        config.proxy = Some(ProxyConfig {
            request: Some(rules(&["X Frame Options"], &[])),
            response: None,
        });
        let findings = validate(&config);
        let proxy: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule == "proxy-header-syntax")
            .collect();
        assert_eq!(proxy.len(), 1, "{findings:?}");
        assert_eq!(proxy[0].severity, Severity::Error);
        assert_eq!(proxy[0].location, "proxy.request");
        assert!(error_summary(&findings).is_some());

        // With a valid header, the proxy rule goes quiet. (The fixture's variant
        // declares no command, so `exactly-one-command` still fires — that is a
        // different rule, asserted separately.)
        config.proxy = Some(ProxyConfig {
            request: Some(rules(&["Origin"], &[("X-Ok", "value")])),
            response: None,
        });
        assert!(
            !validate(&config)
                .iter()
                .any(|f| f.rule == "proxy-header-syntax")
        );
    }

    /// F5: every place that runs something declares exactly one of
    /// `argv` / `shell` (or the v1/v2 `command`).
    #[test]
    fn exactly_one_command_is_required() {
        // Two at once is ambiguous: `from_keys` would have to pick one.
        let both: VeldConfig = serde_json::from_str(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type": "command", "argv": ["true"], "shell": "true"
            }}}}}"#,
        )
        .unwrap();
        let f = validate(&both);
        let hit = f.iter().find(|f| f.rule == "exactly-one-command").unwrap();
        assert!(hit.message.contains("argv, shell"), "{hit:?}");

        // None at all is a step that silently does nothing.
        let neither: VeldConfig = serde_json::from_str(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type": "command"
            }}}}}"#,
        )
        .unwrap();
        assert!(
            validate(&neither)
                .iter()
                .any(|f| f.rule == "exactly-one-command"
                    && f.message.contains("declares no command"))
        );

        // Exactly one — in either spelling — is quiet, and a `script` variant
        // needs no command at all.
        for variant in [
            r#"{"type":"command","argv":["echo","hi"]}"#,
            r#"{"type":"command","shell":"echo hi"}"#,
            r#"{"type":"command","shell": "echo hi"}"#,
            r#"{"type":"command","script":"scripts/x.sh"}"#,
        ] {
            let cfg: VeldConfig = serde_json::from_str(&format!(
                r#"{{"schemaVersion":"3","name":"t","nodes":{{"a":{{"variants":{{"dev":{variant}}}}}}}}}"#
            ))
            .unwrap();
            assert!(
                !validate(&cfg)
                    .iter()
                    .any(|f| f.rule == "exactly-one-command"),
                "{variant} should be accepted: {:?}",
                validate(&cfg)
            );
        }

        // An http probe legitimately declares no command; a command probe must.
        let probe: VeldConfig = serde_json::from_str(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type": "start_server", "shell": "x",
                "probes": { "readiness": { "type": "http", "path": "/z" },
                            "liveness":  { "type": "command" } }
            }}}}}"#,
        )
        .unwrap();
        let probe_findings = validate(&probe);
        let locs: Vec<&str> = probe_findings
            .iter()
            .filter(|f| f.rule == "exactly-one-command")
            .map(|f| f.location.as_str())
            .collect();
        assert_eq!(locs, vec!["nodes.a.variants.dev.probes.liveness"]);
    }

    // -- F0.1: the parse / validate split -------------------------------------

    /// A semantically-broken config must still *parse*. This is the property
    /// `veld stop` / `status` / `logs` depend on: they read the on-disk config
    /// (for `on_stop` hooks and node metadata) and would otherwise be stranded
    /// by an unrelated typo, leaking containers with no way to clean them up.
    #[test]
    fn parse_accepts_config_that_validate_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.json");
        std::fs::write(
            &path,
            r#"{
                "schemaVersion": "3",
                "name": "t",
                "proxy": { "request": { "remove": ["X Frame Options"] } },
                "nodes": { "a": { "variants": { "local": {
                    "type": "start_server", "shell": "true",
                    "on_stop": { "shell": "echo bye" }
                } } } }
            }"#,
        )
        .unwrap();

        let config = parse_config(&path).expect("structurally valid config must parse");
        assert_eq!(
            config.nodes["a"].variants["local"].on_stop,
            Some(Some(CommandSpec::Shell("echo bye".to_owned()))),
            "the on_stop hook stop needs must still be readable"
        );

        // …and the semantic problem is still caught, just on the paths that
        // emit headers rather than on every subcommand.
        let findings = validate(&config);
        assert!(
            findings.iter().any(|f| f.severity == Severity::Error),
            "validate must still reject it: {findings:?}"
        );
    }

    // -- F0.4: depends_on keys stay literal -----------------------------------

    #[test]
    fn interpolated_depends_on_key_is_error() {
        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "t",
                "nodes": {
                    "api": { "variants": { "dev": {
                        "type": "start_server", "shell": "true",
                        "depends_on": { "db-${veld.branch}": "local" }
                    }}},
                    "web": { "variants": { "dev": {
                        "type": "start_server", "shell": "true",
                        "depends_on": { "api": "${veld.variant}" }
                    }}}
                }
            }"#,
        )
        .unwrap();

        let findings = validate(&config);
        let deps: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule == "depends-on-literal")
            .collect();
        assert_eq!(deps.len(), 2, "both the key and the variant: {findings:?}");
        assert!(deps.iter().all(|f| f.severity == Severity::Error));
        // The message must name the offending value so the fix is obvious.
        assert!(
            deps.iter().any(|f| f.message.contains("db-${veld.branch}")),
            "{deps:?}"
        );
        assert!(
            deps.iter().any(|f| f.message.contains("${veld.variant}")),
            "{deps:?}"
        );
    }

    #[test]
    fn literal_depends_on_is_accepted() {
        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "t",
                "nodes": {
                    "db": { "variants": { "local": { "type": "command", "shell": "true" }}},
                    "api": { "variants": { "dev": {
                        "type": "start_server", "shell": "true",
                        "depends_on": { "db": "local" }
                    }}}
                }
            }"#,
        )
        .unwrap();
        assert!(
            !validate(&config)
                .iter()
                .any(|f| f.rule == "depends-on-literal")
        );
    }

    /// The guard for F0.2's namespace closure.
    ///
    /// `${veld.exit_code}` in an `on_stop` hook is the case that made this
    /// necessary: veld's own docs used to recommend it, and after the closure it
    /// resolved to nothing — `run_on_stop_hook` could only log and skip the hook,
    /// so the container never got removed while `veld stop` reported success.
    /// Catching the *name* here turns a silent teardown skip into a refusal at
    /// `veld start`, before anything exists that would need tearing down.
    #[test]
    fn unknown_builtin_var_is_error_and_names_the_replacement() {
        let config: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3",
                "name": "t",
                "nodes": { "db": { "variants": { "dev": {
                    "type": "command",
                    "shell": "true",
                    "outputs": ["CONTAINER"],
                    "on_stop": "docker rm -f ${veld.CONTAINER}-${veld.exit_code}"
                }}}}
            }"#,
        )
        .unwrap();

        let findings = validate(&config);
        let hits: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule == "unknown-builtin-var")
            .collect();
        assert_eq!(hits.len(), 2, "both bad names: {findings:?}");
        assert!(hits.iter().all(|f| f.severity == Severity::Error));
        assert!(
            hits.iter()
                .all(|f| f.location == "nodes.db.variants.dev.on_stop"),
            "{hits:?}"
        );
        // The message has to say what to write instead, or the author is stuck.
        let msg = &hits[0].message;
        assert!(msg.contains("${output."), "{msg}");
        assert!(msg.contains("${nodes.<node>."), "{msg}");
    }

    /// Every real builtin passes the same rule — otherwise it would reject the
    /// configs it is meant to protect.
    #[test]
    fn all_builtin_vars_are_accepted() {
        let refs: String = BUILTIN_VARS
            .iter()
            .map(|n| format!("${{veld.{n}}} "))
            .collect();
        let config: VeldConfig = serde_json::from_str(&format!(
            r#"{{
                "schemaVersion": "3",
                "name": "t",
                "nodes": {{ "a": {{ "variants": {{ "dev": {{
                    "type": "start_server",
                    "shell": "echo {}"
                }}}}}}}}
            }}"#,
            refs.trim()
        ))
        .unwrap();
        let findings = validate(&config);
        assert!(
            !findings.iter().any(|f| f.rule == "unknown-builtin-var"),
            "{findings:?}"
        );
    }

    // -- Schema drift gate -----------------------------------------------------

    /// The other half of the schema drift gate.
    ///
    /// `schema/v3/veld.schema.json` is hand-maintained with no compiler check tying
    /// it to these Rust types, so it drifts silently — and a schema that has drifted
    /// is worse than none, because the editor confidently reports the wrong thing.
    ///
    /// `schema/v3/examples/*.json` are the pin. This test deserializes every one of
    /// them with serde; `tests/validate-schema.sh` validates the same files against
    /// the schema in CI. A change to the types that the schema does not know about
    /// fails there; a change to the schema that the types do not accept fails here.
    /// Either way, adding a field means touching both.
    #[test]
    fn schema_v3_examples_round_trip() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .join("schema/v3/examples");
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{} must exist: {e}", dir.display()))
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .collect();
        assert!(
            !entries.is_empty(),
            "the drift gate is only a gate if there are examples in {}",
            dir.display()
        );

        for entry in entries {
            let path = entry.path();
            let label = path.file_name().unwrap().to_string_lossy().into_owned();

            // The serde side: it must load…
            let cfg =
                parse_config(&path).unwrap_or_else(|e| panic!("{label} must deserialize: {e}"));
            assert_eq!(cfg.schema_version, "3", "{label}");

            // …every node must resolve…
            for (node, node_cfg) in &cfg.nodes {
                for variant in node_cfg.variants.keys() {
                    cfg.resolved(node, variant)
                        .unwrap_or_else(|| panic!("{label}: {node}:{variant} must resolve"));
                }
            }

            // …it must be semantically valid, since a documented example that
            // `veld start` would refuse is not an example…
            let findings = validate(&cfg);
            // An example that declares `include` is a *root* file: its nodes live in
            // files this test cannot load, because the globs point into a project
            // layout that does not exist under `schema/v3/examples/`. So a preset
            // selecting one of those nodes cannot resolve here, while in a real
            // project `parse_config` merges the included files before `validate` ever
            // runs. Only those two rules are exempted, only for such an example —
            // everything else, including preset cycles and dangling `@refs`, still
            // has to hold.
            // `include` is consumed by merging, so it is not on the merged
            // `VeldConfig` — read it off the raw document instead.
            // Comments stripped first: an example is a veld config, so it is
            // JSONC. Reading it here as strict JSON made this half of the drift
            // gate reject a file the loader above had just accepted.
            let source = std::fs::read_to_string(&path).expect("readable");
            let raw: serde_json::Value =
                serde_json::from_str(&crate::jsonc::strip(&source).expect("valid JSONC"))
                    .expect("valid JSON");
            let root_with_includes = raw
                .get("include")
                .and_then(|i| i.as_array())
                .is_some_and(|i| !i.is_empty());
            let errors: Vec<&Finding> = findings
                .iter()
                .filter(|f| f.severity == Severity::Error)
                .filter(|f| {
                    !(root_with_includes
                        && matches!(
                            f.rule.as_str(),
                            "preset-unknown-node" | "preset-unknown-variant"
                        ))
                })
                .collect();
            assert!(errors.is_empty(), "{label} must be valid, got {errors:#?}");

            // …and it must survive a serialize/deserialize round-trip, so nothing in
            // the document is silently dropped on the way through.
            let reserialized = serde_json::to_string(&cfg).expect("serializes");
            let round: VeldConfig = serde_json::from_str(&reserialized)
                .unwrap_or_else(|e| panic!("{label} must round-trip: {e}"));
            assert_eq!(round.nodes.len(), cfg.nodes.len(), "{label}");
            assert_eq!(
                round.hooks, cfg.hooks,
                "{label}: reserved keys must survive"
            );
            assert_eq!(round.ui, cfg.ui, "{label}: reserved keys must survive");
            assert_eq!(
                round.vars.as_ref().map(|v| v.len()),
                cfg.vars.as_ref().map(|v| v.len()),
                "{label}"
            );
        }
    }

    // -- F8: reserved namespaces ----------------------------------------------

    /// `hooks` and `ui` parse, round-trip, and produce the not-executed notice.
    ///
    /// The notice is the point of reserving them: an author who writes a
    /// `worktree.created` hook and sees nothing happen otherwise cannot tell a
    /// not-yet-implemented feature from a config mistake.
    #[test]
    fn reserved_namespaces_are_held_and_reported_as_not_executed() {
        let cfg: VeldConfig = serde_json::from_str(
            r#"{
                "schemaVersion": "3", "name": "t",
                "hooks": {
                    "worktree.created": [ { "argv": ["./scripts/setup-worktree.sh"] } ],
                    "run.stopped":      [ { "shell": "./scripts/collect.sh" } ]
                },
                "ui": { "my-ext": { "title": "Mine", "panel": "p", "commands": [] } },
                "nodes": {}
            }"#,
        )
        .unwrap();

        // Held opaquely — veld does not interpret the shape.
        assert_eq!(cfg.hooks.as_ref().unwrap().as_object().unwrap().len(), 2);
        assert!(cfg.ui.as_ref().unwrap().get("my-ext").is_some());

        // Round-trips, so `veld config` cannot silently drop them.
        let round: VeldConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(round.hooks, cfg.hooks);
        assert_eq!(round.ui, cfg.ui);

        let findings = validate(&cfg);
        let notices: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.rule == "reserved-not-executed")
            .collect();
        assert_eq!(notices.len(), 2, "one each for hooks and ui: {findings:?}");
        assert!(
            notices.iter().all(|f| f.severity == Severity::Notice),
            "a legitimate declaration is not a warning: {notices:?}"
        );
        assert!(
            notices
                .iter()
                .any(|f| f.location == "hooks" && f.message.contains("does not run them")),
            "{notices:?}"
        );
        // …and never blocks a run.
        assert!(error_summary(&findings).is_none());
    }

    /// Reserving two keys must not open the door to every other typo — but the
    /// diagnostic is a **finding**, not a load failure.
    ///
    /// `deny_unknown_fields` would put the failure on the loader that `veld stop`
    /// uses, and would be a regression for v1/v2 documents which previously
    /// ignored an unknown key silently. See `crate::include::Document`.
    #[test]
    fn unknown_top_level_key_is_reported_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.json");
        std::fs::write(
            &path,
            r#"{ "schemaVersion": "3", "name": "t", "hoks": {}, "nodes": {} }"#,
        )
        .unwrap();

        let config = parse_config(&path).expect("a typo must not strand `veld stop`");
        let findings = validate(&config);
        let hit = findings
            .iter()
            .find(|f| f.rule == "unknown-top-level-key")
            .unwrap_or_else(|| panic!("expected the rule to fire: {findings:?}"));
        assert_eq!(hit.severity, Severity::Error);
        assert!(hit.message.contains("\"hoks\""), "{hit:?}");
        // Names `hooks` among the valid keys, so the typo is obvious.
        assert!(hit.message.contains("hooks"), "{hit:?}");
        // …and it blocks `veld start`.
        assert!(error_summary(&findings).is_some());
    }

    // -- F4: vars (a failing case per rule) ------------------------------------

    /// Rule 1: a var is a scalar or a single value source — never an object.
    /// Enforced by the type, so this asserts the type actually refuses.
    #[test]
    fn vars_cannot_hold_object() {
        // A probe block in a var would make this a template system.
        for body in [
            r#"{ "probes": { "readiness": { "type": "http" } } }"#,
            r#"{ "type": "http", "path": "/z" }"#,
            r#"[1, 2, 3]"#,
        ] {
            let json =
                format!(r#"{{"schemaVersion":"3","name":"t","vars":{{"x":{body}}},"nodes":{{}}}}"#);
            assert!(
                serde_json::from_str::<VeldConfig>(&json).is_err(),
                "a var must not accept {body}"
            );
        }
        // …while every legitimate form is accepted.
        for body in [
            r#""https://api.example.com""#,
            r#"{ "value": "devpassword", "secret": true }"#,
            r#"{ "env": "TOKEN", "secret": true }"#,
            r#"{ "file": ".secrets/k" }"#,
            r#"{ "argv": ["secret-tool", "read", "p"], "secret": true }"#,
        ] {
            let json =
                format!(r#"{{"schemaVersion":"3","name":"t","vars":{{"x":{body}}},"nodes":{{}}}}"#);
            serde_json::from_str::<VeldConfig>(&json)
                .unwrap_or_else(|e| panic!("{body} should be a valid var: {e}"));
        }
    }

    /// Rule 2: one hop, always.
    #[test]
    fn vars_cannot_nest() {
        let f = findings_for(
            r#"{"schemaVersion":"3","name":"t",
                "vars":{"base":"https://api.example.com","derived":"${vars.base}/v1"},
                "nodes":{}}"#,
        );
        let hit = f
            .iter()
            .find(|f| f.rule == "vars-cannot-nest")
            .unwrap_or_else(|| panic!("expected the rule to fire: {f:?}"));
        assert_eq!(hit.severity, Severity::Error);
        assert_eq!(hit.location, "vars.derived");
        assert!(hit.message.contains("one hop"), "{hit:?}");
    }

    /// Rule 3: a duplicate var name is a hard error. Two entries of the same name
    /// in one `vars` object is exactly the duplicate-key case.
    #[test]
    fn vars_duplicate_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.json");
        std::fs::write(
            &path,
            r#"{
                "schemaVersion": "3", "name": "t",
                "vars": { "api": "https://one.example.com", "api": "https://two.example.com" },
                "nodes": {}
            }"#,
        )
        .unwrap();
        let cfg = parse_config(&path).expect("still loads — see F0.1");
        let f = validate(&cfg);
        assert!(
            f.iter()
                .any(|f| f.rule == "duplicate-key" && f.message.contains("\"api\"")),
            "{f:?}"
        );
    }

    /// Rule 4: an unknown reference is a hard error listing the declared names —
    /// the cause is almost always a typo, and the fix is seeing the real list.
    #[test]
    fn vars_unknown_is_error() {
        let f = findings_for(
            r#"{"schemaVersion":"3","name":"t",
                "vars":{"remote_api":"https://api.example.com","health_path":"/healthz"},
                "nodes":{"a":{"variants":{"dev":{
                  "type":"command",
                  "shell":"curl ${vars.remote_apo}${vars.health_path}"
                }}}}}"#,
        );
        let hit = f
            .iter()
            .find(|f| f.rule == "unknown-var")
            .unwrap_or_else(|| panic!("expected the rule to fire: {f:?}"));
        assert_eq!(hit.severity, Severity::Error);
        assert!(hit.message.contains("remote_apo"), "{hit:?}");
        // The declared names, so the typo is obvious.
        assert!(hit.message.contains("health_path"), "{hit:?}");
        assert!(hit.message.contains("remote_api"), "{hit:?}");
        // The correctly-spelled one does not fire.
        assert_eq!(
            f.iter().filter(|f| f.rule == "unknown-var").count(),
            1,
            "{f:?}"
        );
    }

    /// A var referenced from every position that supports it resolves quietly.
    #[test]
    fn declared_vars_are_accepted_everywhere() {
        let f = findings_for(
            r#"{"schemaVersion":"3","name":"t",
                "vars":{"api":"https://api.example.com","pw":{"value":"dev","secret":true}},
                "env":{"PROJECT_API":"${vars.api}"},
                "nodes":{"a":{
                  "env":{"NODE_API":"${vars.api}"},
                  "variants":{"dev":{
                    "type":"command",
                    "env":{"VARIANT_API":"${vars.api}"},
                    "argv":["curl","${vars.api}"],
                    "on_stop":{"shell":"echo ${vars.api}"},
                    "skip_if":{"shell":"test -n '${vars.api}'"}
                  }}}}}"#,
        );
        assert!(
            !f.iter().any(|f| f.rule == "unknown-var"),
            "declared vars must be accepted: {f:?}"
        );
    }

    // -- F7: sensitivity lint rules (a passing and a failing fixture each) -----

    fn findings_for(json: &str) -> Vec<Finding> {
        let cfg: VeldConfig = serde_json::from_str(json).expect("fixture parses");
        validate(&cfg)
    }

    /// **error** — a value marked `secret` interpolated into `argv` or `shell`.
    ///
    /// Both forms put the value in the process table, readable by every other
    /// user on the machine, and in any CI log that echoes the command.
    #[test]
    fn secret_in_command_is_validation_error() {
        // Failing fixture: the secret reaches an `argv` element and a `shell`
        // string, plus `on_stop`.
        let bad = findings_for(
            r#"{
                "schemaVersion": "3", "name": "t",
                "nodes": { "db": { "variants": { "dev": {
                    "type": "command",
                    "env": { "PGPASSWORD": { "value": "devpw", "secret": true } },
                    "argv": ["psql", "--password", "${PGPASSWORD}"],
                    "on_stop": { "shell": "echo $PGPASSWORD | wc -c" }
                }}}}
            }"#,
        );
        let hits: Vec<&Finding> = bad
            .iter()
            .filter(|f| f.rule == "secret-in-command")
            .collect();
        assert_eq!(hits.len(), 2, "the command and the on_stop hook: {bad:?}");
        assert!(hits.iter().all(|f| f.severity == Severity::Error));
        assert!(hits.iter().any(|f| f.location.ends_with(".on_stop")));
        assert!(hits[0].message.contains("process table"), "{:?}", hits[0]);

        // Passing fixture: the same secret, delivered as an environment variable
        // and read by the program — the sanctioned route.
        let good = findings_for(
            r#"{
                "schemaVersion": "3", "name": "t",
                "nodes": { "db": { "variants": { "dev": {
                    "type": "command",
                    "env": { "PGPASSWORD": { "value": "devpw", "secret": true } },
                    "argv": ["psql", "--no-password"]
                }}}}
            }"#,
        );
        assert!(
            !good.iter().any(|f| f.rule == "secret-in-command"),
            "{good:?}"
        );

        // A NON-secret value in a command is fine — the rule keys off the flag,
        // not off the name.
        let plain = findings_for(
            r#"{
                "schemaVersion": "3", "name": "t",
                "nodes": { "db": { "variants": { "dev": {
                    "type": "command",
                    "env": { "REGION": "eu-central-1" },
                    "argv": ["deploy", "--region", "${REGION}"]
                }}}}
            }"#,
        );
        assert!(!plain.iter().any(|f| f.rule == "secret-in-command"));
    }

    /// Every way a preset can be broken is reported by `veld lint`.
    ///
    /// All four of these used to report *"is valid"*: `expand_preset` catches an
    /// unknown reference and a cycle, but only when `veld start` runs it, and the
    /// node/variant existence check happened later still, during graph
    /// construction. A preset is edited by hand and used weeks later — exactly the
    /// thing that must fail at lint time rather than at 9am on a Monday.
    #[test]
    fn broken_presets_are_lint_errors_not_start_time_surprises() {
        let f = findings_for(
            r#"{
                "schemaVersion": "3", "name": "t",
                "presets": {
                    "dangling": ["@nope"],
                    "cyclic":   ["@other"],
                    "other":    ["@cyclic"],
                    "ghost":    ["nosuch:dev"],
                    "badvar":   ["api:missing"],
                    "fine":     ["api:dev", "@fine-inner"],
                    "fine-inner": ["api:dev"]
                },
                "nodes": { "api": { "variants": { "dev": {
                    "type": "command", "argv": ["true"]
                }}}}
            }"#,
        );
        let by_location = |loc: &str| -> Vec<&str> {
            f.iter()
                .filter(|x| x.location == loc)
                .map(|x| x.rule.as_str())
                .collect()
        };
        assert_eq!(by_location("presets.dangling"), ["preset-unresolvable"]);
        assert_eq!(by_location("presets.cyclic"), ["preset-unresolvable"]);
        assert_eq!(by_location("presets.ghost"), ["preset-unknown-node"]);
        assert_eq!(by_location("presets.badvar"), ["preset-unknown-variant"]);

        // A valid preset — including one composing another with `@` — stays silent,
        // or the rule is noise that trains people to ignore lint output.
        assert!(by_location("presets.fine").is_empty(), "{f:?}");
        assert!(by_location("presets.fine-inner").is_empty(), "{f:?}");

        // The unknown-variant message lists what does exist, so the fix is visible
        // without opening the config.
        let msg = &f
            .iter()
            .find(|x| x.rule == "preset-unknown-variant")
            .expect("expected the finding")
            .message;
        assert!(msg.contains("it has: dev"), "{msg}");
    }

    /// Every way a preset *key* can be broken or ambiguous is a lint finding.
    ///
    /// A pinned key is a promise — it is what somebody memorised, wrote in a
    /// runbook, or said out loud — so the ways that promise can silently mean
    /// something else all have to fail here rather than at the picker, where the
    /// only symptom is the wrong environment starting.
    #[test]
    fn broken_preset_keys_are_lint_findings() {
        let f = findings_for(
            r#"{
                "schemaVersion": "3", "name": "t",
                "presets": {
                    "dupe-a": { "key": 2, "selections": ["api:dev"] },
                    "dupe-b": { "key": 2, "selections": ["api:dev"] },
                    "zero":   { "key": 0, "selections": ["api:dev"] },
                    "owner":  { "key": 7, "selections": ["api:dev"] },
                    "7":      { "key": 9, "selections": ["api:dev"] },
                    "fine":   { "key": 3, "selections": ["api:dev"] }
                },
                "default_preset": "ghost",
                "nodes": { "api": { "variants": { "dev": {
                    "type": "command", "argv": ["true"]
                }}}}
            }"#,
        );
        let rules = |loc: &str| -> Vec<&str> {
            f.iter()
                .filter(|x| x.location == loc)
                .map(|x| x.rule.as_str())
                .collect()
        };

        // Both sides of a duplicate are named, so the reader does not have to
        // find the other one themselves.
        assert_eq!(rules("presets.dupe-a"), ["preset-duplicate-key"]);
        assert_eq!(rules("presets.dupe-b"), ["preset-duplicate-key"]);
        assert!(
            f.iter()
                .find(|x| x.location == "presets.dupe-a")
                .unwrap()
                .message
                .contains("dupe-b"),
            "{f:?}"
        );

        assert_eq!(rules("presets.zero"), ["preset-invalid-key"]);
        assert_eq!(rules("presets.7"), ["preset-name-shadowed-by-key"]);
        assert_eq!(rules("default_preset"), ["default-preset-unknown"]);
        assert!(rules("presets.fine").is_empty(), "{f:?}");
        assert!(rules("presets.owner").is_empty(), "{f:?}");

        // The shadowing warning must not send someone off to rename a preset —
        // that breaks `--preset <name>` in every script they have. It stays
        // reachable by its own key.
        let shadow = f
            .iter()
            .find(|x| x.rule == "preset-name-shadowed-by-key")
            .expect("expected the finding");
        assert!(shadow.message.contains('9'), "{}", shadow.message);

        // `default_preset` naming nothing must list what does exist.
        let dp = f
            .iter()
            .find(|x| x.rule == "default-preset-unknown")
            .expect("expected the finding");
        assert!(dp.message.contains("fine"), "{}", dp.message);
    }

    /// The undocumented-presets notice fires once, and only once a list is big
    /// enough that picking from it is the problem.
    ///
    /// A notice per preset, or one on a three-preset config, is exactly the noise
    /// that teaches people to ignore lint output — and the array form is fully
    /// supported, so a small config saying `"dev": ["web:dev"]` is not doing
    /// anything wrong.
    #[test]
    fn undocumented_presets_notice_fires_once_and_only_when_crowded() {
        let bare = |count: usize, documented: bool| -> String {
            let mut entries: Vec<String> = (0..count)
                .map(|i| format!("\"p{i}\": [\"api:dev\"]"))
                .collect();
            if documented {
                entries[0] =
                    "\"p0\": { \"label\": \"First\", \"selections\": [\"api:dev\"] }".to_owned();
            }
            format!(
                r#"{{ "schemaVersion": "3", "name": "t",
                      "presets": {{ {} }},
                      "nodes": {{ "api": {{ "variants": {{ "dev": {{
                          "type": "command", "argv": ["true"]
                      }}}}}}}} }}"#,
                entries.join(", ")
            )
        };
        let notices = |src: &str| -> usize {
            findings_for(src)
                .iter()
                .filter(|x| x.rule == "presets-undocumented")
                .count()
        };

        assert_eq!(notices(&bare(7, false)), 0, "7 presets is still scannable");
        assert_eq!(
            notices(&bare(8, false)),
            1,
            "one notice for the whole config, never one per preset"
        );
        assert_eq!(
            notices(&bare(20, true)),
            0,
            "a single documented preset shows the author knows about the object form"
        );
    }

    /// A credential-shaped `proxy` header value warns at every level.
    ///
    /// The `Bearer <token>` case is the one that matters and the one the first
    /// version of this rule missed: the plain detector is whole-string, so it saw
    /// `Bearer ghp_…` and reported clean — a lint that existed and did nothing on
    /// the only header anyone actually sets a credential in.
    #[test]
    fn credential_shaped_proxy_headers_warn_at_every_level() {
        let f = findings_for(
            r#"{
                "schemaVersion": "3", "name": "t",
                "proxy": { "request": { "set": {
                    "Authorization": "Bearer ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
                    "X-Env": "production"
                }}},
                "nodes": { "a": {
                    "proxy": { "request": { "set": { "X-Node": "sk-abcdefghijklmnopqrstuvwxyz" } } },
                    "variants": { "dev": {
                        "type": "command", "argv": ["run"],
                        "proxy": { "response": { "set": {
                            "X-V": "Basic eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc"
                        }}}
                    }}
                }}
            }"#,
        );
        let hits: Vec<&str> = f
            .iter()
            .filter(|x| x.rule == "credential-shaped-proxy-header")
            .map(|x| x.location.as_str())
            .collect();
        for expected in [
            "proxy.request.set.Authorization",
            "nodes.a.proxy.request.set.X-Node",
            "nodes.a.variants.dev.proxy.response.set.X-V",
        ] {
            assert!(hits.contains(&expected), "missing {expected}: {hits:?}");
        }
        // A plain value must stay quiet, or the rule is noise nobody can act on.
        assert!(
            !hits.iter().any(|h| h.ends_with("X-Env")),
            "false positive on a non-credential value: {hits:?}"
        );
        // Warning, not error: the value may be a deliberate local-only credential,
        // and this must not block `veld start`.
        assert!(
            f.iter()
                .filter(|x| x.rule == "credential-shaped-proxy-header")
                .all(|x| x.severity == Severity::Warning)
        );
    }

    /// `${nodes.<other>.KEY}` is judged by **that** node's declaration.
    ///
    /// The rule used to take the trailing field of a cross-node reference and match
    /// it against the *current* variant's secret set, so one node calling an output
    /// `DATABASE_URL` made every other node's `DATABASE_URL` unusable in a command —
    /// and said it "is declared secret" about a value nobody declared. A doc example
    /// in `docs/scenarios.md` tripped it. Both directions are pinned here: a false
    /// positive on a security rule teaches people to route around the linter, and
    /// under-reporting hides a real leak.
    #[test]
    fn cross_node_output_refs_follow_the_producing_node() {
        // `pg` publishes a plain URL; `clone` marks its *own* same-named output
        // sensitive. Reading `${nodes.pg.DATABASE_URL}` is fine.
        let shared_name = findings_for(
            r#"{
                "schemaVersion": "3", "name": "t",
                "nodes": {
                    "pg": { "variants": { "dev": {
                        "type": "start_server", "shell": "postgres",
                        "probes": { "readiness": { "type": "port" } },
                        "outputs": { "DATABASE_URL": "postgres://localhost:${veld.port}/x" }
                    }}},
                    "clone": { "variants": { "dev": {
                        "type": "command",
                        "shell": "pg_dump | psql ${nodes.pg.DATABASE_URL}",
                        "outputs": ["DATABASE_URL"],
                        "sensitive_outputs": ["DATABASE_URL"]
                    }}}
                }
            }"#,
        );
        assert!(
            !shared_name.iter().any(|f| f.rule == "secret-in-command"),
            "same output name in another node must not be treated as secret: {shared_name:?}"
        );

        // But when the *producing* node marks it sensitive, the reference is a leak.
        let real_leak = findings_for(
            r#"{
                "schemaVersion": "3", "name": "t",
                "nodes": {
                    "vault": { "variants": { "dev": {
                        "type": "command", "shell": "issue-token",
                        "outputs": ["TOKEN"],
                        "sensitive_outputs": ["TOKEN"]
                    }}},
                    "app": { "variants": { "dev": {
                        "type": "command",
                        "argv": ["deploy", "--token", "${nodes.vault.TOKEN}"]
                    }}}
                }
            }"#,
        );
        let leak = real_leak
            .iter()
            .find(|f| f.rule == "secret-in-command")
            .unwrap_or_else(|| panic!("expected a leak finding, got {real_leak:?}"));
        assert!(leak.location.starts_with("nodes.app"), "{leak:?}");
        // The remedy must not claim veld already exports another node's output.
        assert!(leak.message.contains("nodes.vault.TOKEN"), "{leak:?}");
    }

    /// **warn** — a credential-shaped literal, marked or not.
    /// **silent** — a `secret: true` literal that is not credential-shaped, which
    /// is the legitimate fixed-local-credential case.
    #[test]
    fn credential_shaped_literals_warn_and_plain_ones_stay_silent() {
        for shaped in [
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
            "sk-abcdefghijklmnopqrstuvwxyz",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc",
            "postgres://admin:sup3rs3cretpw@db.example.com/app",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            let f = findings_for(&format!(
                r#"{{"schemaVersion":"3","name":"t","nodes":{{"a":{{"variants":{{"dev":{{
                    "type":"command","shell":"true","env":{{"TOKEN":{}}}
                }}}}}}}}}}"#,
                serde_json::to_string(shaped).unwrap()
            ));
            let hit = f
                .iter()
                .find(|f| f.rule == "credential-shaped-literal")
                .unwrap_or_else(|| panic!("{shaped} should warn, got {f:?}"));
            assert_eq!(hit.severity, Severity::Warning, "never blocks a run");
        }

        // The legitimate case: a fixed local credential, deliberately inline and
        // marked. Silent — this is how local dev works and nagging about it
        // trains people to ignore the warning.
        let quiet = findings_for(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"command","shell":"true",
                "env":{"PG_PASSWORD":{"value":"devpassword","secret":true}}
            }}}}}"#,
        );
        assert!(
            !quiet.iter().any(|f| f.rule == "credential-shaped-literal"),
            "{quiet:?}"
        );

        // …and so are ordinary values that merely contain a URL or a colon.
        for benign in [
            "eu-central-1",
            "https://api.example.com",
            "postgres://user:postgres@localhost:5432/app",
            "debug",
        ] {
            let f = findings_for(&format!(
                r#"{{"schemaVersion":"3","name":"t","nodes":{{"a":{{"variants":{{"dev":{{
                    "type":"command","shell":"true","env":{{"V":{}}}
                }}}}}}}}}}"#,
                serde_json::to_string(benign).unwrap()
            ));
            assert!(
                !f.iter().any(|f| f.rule == "credential-shaped-literal"),
                "{benign} should stay quiet, got {f:?}"
            );
        }
    }

    /// **error** — a `start_server` with no readiness probe. One schema version, so
    /// one severity.
    #[test]
    fn start_server_without_readiness_is_error() {
        let v3 = findings_for(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"start_server","shell":"serve"
            }}}}}"#,
        );
        let hit = v3
            .iter()
            .find(|f| f.rule == "start-server-needs-readiness")
            .unwrap();
        assert_eq!(hit.severity, Severity::Error);

        // Passing fixture: either probe form satisfies it.
        for probe in [
            r#""probes":{"readiness":{"type":"http","path":"/z"}}"#,
            r#""probes":{"readiness":{"type":"port"}}"#,
            r#""health_check":{"type":"port"}"#,
        ] {
            let f = findings_for(&format!(
                r#"{{"schemaVersion":"3","name":"t","nodes":{{"a":{{"variants":{{"dev":{{
                    "type":"start_server","shell":"serve",{probe}
                }}}}}}}}}}"#
            ));
            assert!(
                !f.iter().any(|f| f.rule == "start-server-needs-readiness"),
                "{probe} should satisfy the rule, got {f:?}"
            );
        }
    }

    #[test]
    fn ambiguous_primary_port_is_error() {
        let f = findings_for(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"start_server","shell":"x",
                "probes":{"readiness":{"type":"port"}},
                "ports":{"grpc":"auto","metrics":"auto"}
            }}}}}"#,
        );
        let hit = f
            .iter()
            .find(|f| f.rule == "ambiguous-primary-port")
            .unwrap_or_else(|| panic!("expected the rule to fire: {f:?}"));
        assert_eq!(hit.severity, Severity::Error);
        assert!(hit.message.contains("grpc, metrics"), "{hit:?}");
        assert!(hit.message.contains("http"), "must say how to fix it");

        // Naming one `http` resolves it.
        let ok = findings_for(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"start_server","shell":"x",
                "probes":{"readiness":{"type":"port"}},
                "ports":{"http":"auto","metrics":"auto"}
            }}}}}"#,
        );
        assert!(!ok.iter().any(|f| f.rule == "ambiguous-primary-port"));
    }

    #[test]
    fn missing_step_type_is_error() {
        let f = findings_for(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "shell":"x"
            }}}}}"#,
        );
        assert!(f.iter().any(|f| f.rule == "missing-step-type"));

        // Declared once on the node covers every variant.
        let ok = findings_for(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
                "type":"command",
                "variants":{"dev":{"shell":"x"},"ci":{"shell":"y"}}
            }}}"#,
        );
        assert!(!ok.iter().any(|f| f.rule == "missing-step-type"));
    }

    // -- F5: the v3 command gate ----------------------------------------------

    /// A v3 document containing `command` fails to load, and the message says what
    /// to write instead. There is no converter to defer to, so the error is the
    /// whole instruction.
    #[test]
    fn v3_rejects_legacy_command_and_says_what_to_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.json");
        std::fs::write(
            &path,
            r#"{
                "schemaVersion": "3",
                "name": "t",
                "setup": [ { "name": "s", "command": "echo setup" } ],
                "nodes": { "api": { "variants": { "dev": {
                    "type": "start_server",
                    "command": "pnpm dev",
                    "on_stop": "docker rm -f x"
                }}}}
            }"#,
        )
        .unwrap();

        let err = parse_config(&path).unwrap_err();
        let ConfigError::LegacyCommandInV3 { locations, .. } = &err else {
            panic!("expected LegacyCommandInV3, got {err:?}");
        };
        // Every offending position is named, so a large config can be fixed in
        // one pass rather than one error at a time.
        assert_eq!(
            locations,
            &[
                "nodes.api.variants.dev.command".to_owned(),
                // A bare-string `on_stop` is the v1/v2 form too.
                "nodes.api.variants.dev.on_stop".to_owned(),
                "setup[0].command".to_owned(),
            ]
        );
        let msg = err.to_string();
        assert!(msg.contains("argv") && msg.contains("shell"), "{msg}");
        assert!(msg.contains("docs/migrating-to-v3.md"), "{msg}");
        // No converter exists; a message that names one sends the reader nowhere.
        assert!(!msg.contains("--migrate"), "{msg}");
    }

    /// v3 is the only supported version, and the message has to *be* the upgrade
    /// instructions — this is the error every existing user meets exactly once, and
    /// there is no `--migrate` to hand them off to.
    #[test]
    fn v1_and_v2_are_rejected_with_migration_instructions() {
        let dir = tempfile::tempdir().unwrap();
        for version in ["1", "2"] {
            let path = dir.path().join(format!("v{version}.json"));
            std::fs::write(
                &path,
                format!(
                    r#"{{
                        "schemaVersion": "{version}",
                        "name": "t",
                        "nodes": {{ "api": {{ "variants": {{ "dev": {{
                            "type": "start_server", "command": "pnpm dev"
                        }}}}}}}}
                    }}"#
                ),
            )
            .unwrap();
            let err = parse_config(&path).unwrap_err();
            assert!(
                matches!(err, ConfigError::UnsupportedSchemaVersion(ref v) if v == version),
                "v{version}: {err:?}"
            );
            let msg = err.to_string();
            // The message is the whole upgrade path: what changes, where the full
            // rules are, and how to check the result.
            assert!(msg.contains("argv") && msg.contains("shell"), "{msg}");
            assert!(msg.contains("${output.KEY}"), "{msg}");
            assert!(msg.contains("docs/migrating-to-v3.md"), "{msg}");
            assert!(msg.contains("veld lint"), "{msg}");
            // Pointing at a converter veld no longer ships is a dead end.
            assert!(!msg.contains("--migrate"), "{msg}");
        }
    }

    /// The version error outranks **every** other complaint about the document.
    ///
    /// A v1/v2 config is the one most likely to hold a shape the v3 model rejects,
    /// so if typed deserialization ran first, an existing user would meet `missing
    /// field ...` or `unknown variant ...` instead of the upgrade instructions. This
    /// used to be the behaviour. With no converter to stumble into, that error is
    /// the only guidance there is, so the ordering is pinned here rather than left
    /// to the order statements happen to appear in.
    #[test]
    fn version_error_outranks_every_other_parse_complaint() {
        let dir = tempfile::tempdir().unwrap();
        // Each of these fails for a *second*, unrelated reason as well: a node with
        // no `variants`, an unknown node `type`, and a wrongly-typed field.
        for (label, body) in [
            (
                "no variants",
                r#""nodes": { "web": { "command": "x", "port": 3000 } }"#,
            ),
            (
                "unknown type",
                r#""nodes": { "web": { "variants": { "dev": { "type": "nope" } } } }"#,
            ),
            ("bad field type", r#""nodes": { "web": 42 }"#),
        ] {
            let path = dir.path().join("veld.json");
            std::fs::write(
                &path,
                format!(r#"{{ "schemaVersion": "2", "name": "t", {body} }}"#),
            )
            .unwrap();
            let err = parse_config(&path).unwrap_err();
            assert!(
                matches!(err, ConfigError::UnsupportedSchemaVersion(ref v) if v == "2"),
                "{label}: expected the version error to win, got {err:?}"
            );
        }
    }

    /// A v3 document using `argv`/`shell` loads and validates.
    #[test]
    fn v3_accepts_argv_and_shell() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.json");
        std::fs::write(
            &path,
            r#"{
                "schemaVersion": "3",
                "name": "t",
                "setup": [ { "name": "s", "argv": ["echo", "setup"] } ],
                "nodes": { "api": { "variants": { "dev": {
                    "type": "start_server",
                    "argv": ["pnpm", "dev"],
                    "probes": { "readiness": { "type": "http", "path": "/healthz" } },
                    "on_stop": { "shell": "docker rm -f x" }
                }}}}
            }"#,
        )
        .unwrap();
        let cfg = parse_config(&path).expect("v3 argv/shell must load");
        assert_eq!(
            cfg.nodes["api"].variants["dev"].cmd.spec(),
            Some(CommandSpec::Argv(vec!["pnpm".into(), "dev".into()]))
        );
        assert_eq!(
            cfg.nodes["api"].variants["dev"].on_stop,
            Some(Some(CommandSpec::Shell("docker rm -f x".into())))
        );
        assert!(!validate(&cfg).iter().any(|f| f.severity == Severity::Error));
    }

    // -- F1: JSONC -------------------------------------------------------------

    /// A commented, trailing-comma'd config loads, and a syntax error *after* the
    /// comments still reports the position the editor shows — the reason comments
    /// are blanked rather than deleted.
    #[test]
    fn commented_config_loads_and_error_positions_survive() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("veld.json");
        std::fs::write(
            &good,
            r#"{
  // The project this config describes.
  "schemaVersion": "3",
  "name": "commented", /* trailing block comment */
  "nodes": {
    "api": {
      "default_variant": "dev",
      "variants": {
        // A comment-like string must survive: https://example.com//x
        "dev": { "type": "command", "shell": "echo //not-a-comment", },
      },
    },
  },
}"#,
        )
        .unwrap();
        let config = parse_config(&good).expect("JSONC must load");
        assert_eq!(config.name, "commented");
        assert_eq!(
            config.nodes["api"].variants["dev"].cmd.shell.as_deref(),
            Some("echo //not-a-comment")
        );

        // A real syntax error, four comment lines in.
        let bad = dir.path().join("bad.json");
        std::fs::write(
            &bad,
            "{\n  // one\n  /* two\n     three */\n  \"schemaVersion\": nope\n}",
        )
        .unwrap();
        let err = parse_config(&bad).unwrap_err();
        let ConfigError::ParseError { source, .. } = err else {
            panic!("expected a parse error, got {err:?}");
        };
        assert_eq!(
            (source.line(), source.column()),
            (5, 21),
            "position must survive comment stripping: {source}"
        );
    }

    /// A duplicate key is an error (F1) — but a *validation* error, not a load
    /// error (F0.1).
    ///
    /// Two nodes with the same name is the case that matters: `nodes` is a map, so
    /// `serde_json` silently keeps the last and one whole node vanishes with no
    /// diagnostic. (A duplicated *struct field* like two `variants` blocks is
    /// already a parse error from `serde_derive`; that is pre-existing and
    /// unchanged.)
    ///
    /// It must not fail the load, because the loader runs on `veld stop`, which
    /// reads `on_stop` from the on-disk config: failing here would mean teardown
    /// never runs and containers leak. The document is still perfectly
    /// interpretable — last-wins is deterministic — so `stop` proceeds on that
    /// reading while `start` and `lint` refuse.
    #[test]
    fn duplicate_key_within_file_is_validation_error_not_load_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.json");
        std::fs::write(
            &path,
            r#"{
                "schemaVersion": "3",
                "name": "dup",
                "nodes": {
                    "api": { "variants": { "dev": { "type": "command", "shell": "first" } } },
                    "api": { "variants": { "dev": { "type": "command", "shell": "second" } } }
                }
            }"#,
        )
        .unwrap();

        // Loads — so `stop` / `status` / `logs` and the daemon monitor keep working.
        let config = parse_config(&path).expect("a duplicate key must not fail the load");
        assert_eq!(
            config.nodes["api"].variants["dev"].cmd.shell.as_deref(),
            Some("second"),
            "serde_json's last-wins reading is what the rest of veld sees"
        );

        // …and is still an error, on the paths that can afford to refuse.
        let findings = validate(&config);
        let dup = findings
            .iter()
            .find(|f| f.rule == "duplicate-key")
            .unwrap_or_else(|| panic!("expected a duplicate-key finding, got {findings:?}"));
        assert_eq!(dup.severity, Severity::Error);
        assert!(dup.message.contains("duplicate key \"api\""), "{dup:?}");
        assert!(error_summary(&findings).is_some());
    }

    #[test]
    fn unterminated_block_comment_is_named_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.json");
        std::fs::write(&path, "{\n  /* oops\n  \"name\": \"x\"\n}").unwrap();
        let err = parse_config(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Jsonc { .. }), "{err:?}");
        assert!(
            err.to_string().contains("unterminated block comment"),
            "{err}"
        );
    }

    /// Parse failures are strictly structural: unreadable file, malformed JSON,
    /// wrong types, unsupported schema version. Nothing else.
    #[test]
    fn parse_rejects_only_structural_problems() {
        let dir = tempfile::tempdir().unwrap();

        let malformed = dir.path().join("malformed.json");
        std::fs::write(&malformed, "{ not json").unwrap();
        assert!(matches!(
            parse_config(&malformed),
            Err(ConfigError::ParseError { .. })
        ));

        let bad_version = dir.path().join("version.json");
        std::fs::write(
            &bad_version,
            r#"{"schemaVersion":"99","name":"t","nodes":{}}"#,
        )
        .unwrap();
        assert!(matches!(
            parse_config(&bad_version),
            Err(ConfigError::UnsupportedSchemaVersion(_))
        ));

        assert!(matches!(
            parse_config(&dir.path().join("nope.json")),
            Err(ConfigError::ReadError { .. })
        ));
    }

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
        let project = HashMap::from([("A".into(), Some(ConfigValue::literal("1")))]);
        let result = resolve_env(Some(&project), None, None).unwrap();
        assert_eq!(result.get("A").unwrap().as_literal(), Some("1"));
    }

    #[test]
    fn test_resolve_env_node_overrides_project() {
        let project = HashMap::from([
            ("A".into(), Some(ConfigValue::literal("1"))),
            ("B".into(), Some(ConfigValue::literal("2"))),
        ]);
        let node = HashMap::from([("A".into(), Some(ConfigValue::literal("override")))]);
        let result = resolve_env(Some(&project), Some(&node), None).unwrap();
        assert_eq!(result.get("A").unwrap().as_literal(), Some("override"));
        assert_eq!(result.get("B").unwrap().as_literal(), Some("2"));
    }

    #[test]
    fn test_resolve_env_variant_overrides_all() {
        let project = HashMap::from([("A".into(), Some(ConfigValue::literal("1")))]);
        let node = HashMap::from([
            ("A".into(), Some(ConfigValue::literal("2"))),
            ("B".into(), Some(ConfigValue::literal("3"))),
        ]);
        let variant = HashMap::from([
            ("A".into(), Some(ConfigValue::literal("final"))),
            ("C".into(), Some(ConfigValue::literal("4"))),
        ]);
        let result = resolve_env(Some(&project), Some(&node), Some(&variant)).unwrap();
        assert_eq!(result.get("A").unwrap().as_literal(), Some("final"));
        assert_eq!(result.get("B").unwrap().as_literal(), Some("3"));
        assert_eq!(result.get("C").unwrap().as_literal(), Some("4"));
    }

    #[test]
    fn test_resolve_env_empty_map_with_values() {
        let empty = HashMap::new();
        let variant = HashMap::from([("X".into(), Some(ConfigValue::literal("1")))]);
        let result = resolve_env(Some(&empty), None, Some(&variant)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("X").unwrap().as_literal(), Some("1"));
    }

    #[test]
    fn test_resolve_env_all_empty_maps() {
        let empty = HashMap::new();
        let result = resolve_env(Some(&empty), Some(&empty), Some(&empty)).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_env_variant_only() {
        let variant = HashMap::from([("X".into(), Some(ConfigValue::literal("val")))]);
        let result = resolve_env(None, None, Some(&variant)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("X").unwrap().as_literal(), Some("val"));
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
        let json = r#"{"name": "docker", "shell": "docker info", "failureMessage": "Docker must be running"}"#;
        let step: SetupStep = serde_json::from_str(json).unwrap();
        assert_eq!(step.name, "docker");
        assert_eq!(step.cmd.command.as_deref(), None);
        assert_eq!(step.cmd.shell.as_deref(), Some("docker info"));
        assert_eq!(
            step.failure_message.as_deref(),
            Some("Docker must be running")
        );
    }

    #[test]
    fn test_setup_step_without_failure_message() {
        let json = r#"{"name": "network", "shell": "docker network create veld"}"#;
        let step: SetupStep = serde_json::from_str(json).unwrap();
        assert_eq!(step.name, "network");
        assert_eq!(
            step.cmd.shell.as_deref(),
            Some("docker network create veld")
        );
        assert!(step.failure_message.is_none());
    }

    #[test]
    fn test_config_with_setup_and_teardown() {
        let json = r#"{
            "schemaVersion": "3",
            "name": "test-project",
            "setup": [
                {"name": "check", "shell": "echo ok", "failureMessage": "Check failed"},
                {"name": "init", "shell": "mkdir -p /tmp/test"}
            ],
            "teardown": [
                {"name": "cleanup", "shell": "rm -rf /tmp/test"}
            ],
            "nodes": {
                "app": {
                    "variants": {
                        "local": {
                            "type": "start_server",
                            "shell": "echo start"
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
            "schemaVersion": "3",
            "name": "test-project",
            "nodes": {
                "app": {
                    "variants": {
                        "local": {
                            "type": "start_server",
                            "shell": "echo start"
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
                "shell": "pg_isready",
                "interval_ms": 5000,
                "failure_threshold": 5,
                "max_recoveries": 2
            }
        }"#;
        let probes: ProbesConfig = serde_json::from_str(json).unwrap();
        let readiness = probes.readiness.unwrap().unwrap();
        assert_eq!(readiness.check_type, "http");
        assert_eq!(readiness.path.as_deref(), Some("/health"));
        assert_eq!(readiness.timeout_seconds, 30);

        let liveness = probes.liveness.unwrap().unwrap();
        assert_eq!(liveness.check_type, "command");
        assert_eq!(liveness.cmd.shell.as_deref(), Some("pg_isready"));
        assert_eq!(liveness.interval_ms, 5000);
        assert_eq!(liveness.failure_threshold, 5);
        assert_eq!(liveness.max_recoveries, 2);
    }

    #[test]
    fn test_liveness_probe_defaults() {
        let json = r#"{"type": "command", "shell": "true"}"#;
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
            "shell": "echo run",
            "skip_if": "test -f /tmp/done"
        }"#;
        let v: VariantConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            v.skip_if,
            Some(CommandSpec::Shell("test -f /tmp/done".to_owned()))
        );
    }

    #[test]
    fn test_verify_alias_for_skip_if() {
        let json = r#"{
            "type": "command",
            "shell": "echo run",
            "verify": "test -f /tmp/done"
        }"#;
        let v: VariantConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            v.skip_if,
            Some(CommandSpec::Shell("test -f /tmp/done".to_owned()))
        );
    }

    // -- Schema version tests --------------------------------------------------

    #[test]
    fn test_probes_parse_on_a_v3_document() {
        let json = r#"{
            "schemaVersion": "3",
            "name": "test-project",
            "nodes": {
                "db": {
                    "variants": {
                        "local": {
                            "type": "command",
                            "shell": "echo start",
                            "probes": {
                                "liveness": {
                                    "type": "command",
                                    "shell": "pg_isready"
                                }
                            }
                        }
                    }
                }
            }
        }"#;
        let config: VeldConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.schema_version, "3");
        let variant = &config.nodes["db"].variants["local"];
        assert!(variant.probes.is_some());
        let liveness = variant
            .probes
            .as_ref()
            .unwrap()
            .liveness
            .clone()
            .unwrap()
            .unwrap();
        assert_eq!(liveness.check_type, "command");
    }

    // -- Action config tests ---------------------------------------------------

    #[test]
    fn test_action_minimal_deserialization() {
        let json = r#"{"name": "psql", "shell": "psql $DB_URL"}"#;
        let action: ActionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(action.name, "psql");
        assert_eq!(action.cmd.shell.as_deref(), Some("psql $DB_URL"));
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
            "shell": "open -a Postico \"postgresql://${output.DB_USER}@${output.DB_HOST}:${output.DB_PORT}/${output.DB_NAME}\"",
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
            "variants": {"dblab": {"type": "start_server", "shell": "ssh -L ..."}},
            "actions": [
                {"name": "postico", "shell": "open -a Postico", "requires_outputs": ["DB_HOST"]}
            ]
        }"#;
        let node: NodeConfig = serde_json::from_str(json).unwrap();
        let actions = node.actions.unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].name, "postico");
    }

    // -- Readiness probe helper tests ------------------------------------------

    // -- F3: the node -> variant cascade (one test per merge-table row) --------

    /// Build a config from JSON and resolve one node:variant.
    fn resolve(json: &str, node: &str, variant: &str) -> ResolvedVariant {
        let cfg: VeldConfig = serde_json::from_str(json).expect("fixture parses");
        cfg.resolved(node, variant)
            .unwrap_or_else(|| panic!("{node}:{variant} missing"))
    }

    /// `probes.readiness` supersedes the legacy `health_check` *within* a level,
    /// and either beats anything the node said.
    #[test]
    fn readiness_precedence_within_and_across_levels() {
        // Variant `probes.readiness` wins over variant `health_check`.
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"start_server","shell":"x",
                "health_check":{"type":"port"},
                "probes":{"readiness":{"type":"http","path":"/ready"}}
            }}}}}"#,
            "a",
            "dev",
        );
        assert_eq!(r.readiness.as_ref().unwrap().check_type, "http");

        // Variant `health_check` alone still works (v1/v2 configs).
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"start_server","shell":"x","health_check":{"type":"port"}
            }}}}}"#,
            "a",
            "dev",
        );
        assert_eq!(r.readiness.as_ref().unwrap().check_type, "port");

        // Hoisted to the node, inherited by a silent variant.
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
                "probes":{"readiness":{"type":"http","path":"/hoisted"}},
                "variants":{"dev":{"type":"start_server","shell":"x"}}
            }}}"#,
            "a",
            "dev",
        );
        assert_eq!(
            r.readiness.as_ref().unwrap().path.as_deref(),
            Some("/hoisted")
        );
    }

    /// **Merge table: `probes` replaces the whole probe object.**
    ///
    /// A deliberate exception to the per-field pattern `features` uses. A probe is
    /// a tagged union, so field-wise merging would let a variant switch
    /// `type: "http"` to `type: "command"` and silently inherit a stale `path` —
    /// a probe that then checks the wrong thing forever.
    #[test]
    fn probes_are_replaced_wholesale_not_merged() {
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
                "probes":{"readiness":{"type":"http","path":"/from-node","expect_status":204}},
                "variants":{"dev":{
                    "type":"start_server","shell":"x",
                    "probes":{"readiness":{"type":"command","shell":"pg_isready"}}
                }}
            }}}"#,
            "a",
            "dev",
        );
        let readiness = r.readiness.as_ref().unwrap();
        assert_eq!(readiness.check_type, "command");
        assert_eq!(
            readiness.path, None,
            "the node's `path` must NOT survive into a command probe"
        );
        assert_eq!(readiness.expect_status, None);
    }

    /// **Merge table: a variant erases an inherited probe with `null`.**
    #[test]
    fn variant_can_erase_an_inherited_probe() {
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
                "probes":{"liveness":{"type":"command","shell":"pg_isready"}},
                "variants":{"dev":{
                    "type":"start_server","shell":"x",
                    "probes":{"liveness":null}
                }}
            }}}"#,
            "a",
            "dev",
        );
        assert!(r.liveness.is_none(), "\"liveness\": null must erase it");
    }

    /// **Merge table: `env` / `ports` / `depends_on` are additive per key**, the
    /// variant wins on collision, and `"KEY": null` erases.
    #[test]
    fn maps_are_additive_per_key_and_null_erases() {
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t",
                "env":{"FROM_PROJECT":"p","SHARED":"p"},
                "nodes":{
                  "db":{"variants":{"local":{"type":"command","shell":"x"}}},
                  "cache":{"variants":{"local":{"type":"command","shell":"x"}}},
                  "a":{
                    "env":{"FROM_NODE":"n","SHARED":"n","DROPPED":"n"},
                    "ports":{"http":"auto","debug":"auto","gone":"auto"},
                    "depends_on":{"db":"local","cache":"local"},
                    "variants":{"dev":{
                      "type":"start_server","shell":"x",
                      "env":{"SHARED":"v","DROPPED":null},
                      "ports":{"metrics":"auto","gone":null},
                      "depends_on":{"cache":null}
                    }}
                  }}}"#,
            "a",
            "dev",
        );

        let env = r.env.as_ref().unwrap();
        assert_eq!(
            env.get("FROM_PROJECT").and_then(|v| v.as_literal()),
            Some("p")
        );
        assert_eq!(env.get("FROM_NODE").and_then(|v| v.as_literal()), Some("n"));
        assert_eq!(
            env.get("SHARED").and_then(|v| v.as_literal()),
            Some("v"),
            "the variant wins on collision"
        );
        assert!(!env.contains_key("DROPPED"), "null erases an inherited key");

        let ports = r.ports.as_ref().unwrap();
        let names: Vec<&str> = ports.ports.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["debug", "http", "metrics"]);
        assert_eq!(ports.primary, "http");

        let deps = r.depends_on.as_ref().unwrap();
        assert_eq!(deps.get("db").map(String::as_str), Some("local"));
        assert!(
            !deps.contains_key("cache"),
            "a variant can drop an inherited dependency"
        );
    }

    /// **Merge table: `features` is per field**, not wholesale — the other
    /// strategy, kept distinct on purpose.
    #[test]
    fn features_merge_per_field() {
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t",
                "features":{"feedback_overlay":false,"client_logs":false},
                "nodes":{"a":{
                  "features":{"client_logs":true},
                  "variants":{"dev":{"type":"start_server","shell":"x",
                    "features":{"inject":false}}}
                }}}"#,
            "a",
            "dev",
        );
        // Each field resolves independently at its own most-specific level.
        assert!(!r.features.feedback_overlay, "from project");
        assert!(r.features.client_logs, "from node");
        assert!(!r.features.inject, "from variant");
    }

    /// **Merge table: `share` and `outputs` replace wholesale**, and `null`
    /// erases. `share` in particular must never half-inherit — it is a consent
    /// decision.
    #[test]
    fn share_and_outputs_replace_wholesale() {
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
                "share":{"expose":["peer","web"]},
                "outputs":{"URL":"${veld.url}","EXTRA":"x"},
                "variants":{"dev":{"type":"start_server","shell":"x",
                  "share":{"expose":["peer"]},
                  "outputs":{"URL":"${veld.url}"}}}
            }}}"#,
            "a",
            "dev",
        );
        let share = r.share.as_ref().unwrap();
        assert!(share.allows(ExposeMode::Peer));
        assert!(
            !share.allows(ExposeMode::Web),
            "the node's `web` must NOT survive — sharing is replaced, not merged"
        );
        assert_eq!(
            r.outputs.as_ref().unwrap().declared_keys().len(),
            1,
            "the node's EXTRA output must not survive"
        );

        // `null` erases the node-level opt-in entirely.
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
                "share":{"expose":["peer"]},
                "variants":{"dev":{"type":"start_server","shell":"x","share":null}}
            }}}"#,
            "a",
            "dev",
        );
        assert!(r.share.is_none(), "\"share\": null must erase it");
    }

    /// **Merge table: `type`, `on_stop`, and the command replace.** A variant that
    /// states one wins; otherwise the node's applies.
    #[test]
    fn scalars_and_commands_replace() {
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
                "type":"start_server",
                "shell":"node-level",
                "on_stop":{"shell":"node-stop"},
                "variants":{
                  "inherits":{},
                  "overrides":{"type":"command","argv":["own"],"on_stop":{"shell":"own-stop"}}
                }
            }}}"#,
            "a",
            "inherits",
        );
        assert_eq!(r.step_type, StepType::StartServer);
        assert_eq!(r.command, Some(CommandSpec::Shell("node-level".into())));
        assert_eq!(r.on_stop, Some(CommandSpec::Shell("node-stop".into())));

        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
                "type":"start_server",
                "shell":"node-level",
                "on_stop":{"shell":"node-stop"},
                "variants":{
                  "inherits":{},
                  "overrides":{"type":"command","argv":["own"],"on_stop":{"shell":"own-stop"}}
                }
            }}}"#,
            "a",
            "overrides",
        );
        assert_eq!(r.step_type, StepType::Command);
        assert_eq!(r.command, Some(CommandSpec::Argv(vec!["own".into()])));
        assert_eq!(r.on_stop, Some(CommandSpec::Shell("own-stop".into())));
    }

    /// `"on_stop": null` on a variant **erases** the node's hook.
    ///
    /// It used to inherit it — and then *run* it, because a plain `Option` cannot
    /// tell an explicit `null` from an absent key. An author disabling a teardown
    /// command got the command. This asserts the three-way distinction, including
    /// that absent still inherits, since the erase must not be achieved by breaking
    /// inheritance for everyone.
    #[test]
    fn explicit_null_on_stop_erases_the_node_hook() {
        let config = r#"{"schemaVersion":"3","name":"t","nodes":{"a":{
            "type":"command",
            "argv":["run"],
            "on_stop":{"shell":"node-stop"},
            "variants":{
              "absent":{},
              "erased":{"on_stop":null},
              "own":{"on_stop":{"shell":"own-stop"}}
            }
        }}}"#;
        assert_eq!(
            resolve(config, "a", "absent").on_stop,
            Some(CommandSpec::Shell("node-stop".into())),
            "an absent on_stop must still inherit the node's hook"
        );
        assert_eq!(
            resolve(config, "a", "erased").on_stop,
            None,
            "an explicit null must erase the node's hook, not inherit it"
        );
        assert_eq!(
            resolve(config, "a", "own").on_stop,
            Some(CommandSpec::Shell("own-stop".into()))
        );
    }

    /// **Merge table: `proxy` is pre-existing and must not change.** Its union
    /// semantics are asserted directly against `resolve_proxy` elsewhere; this
    /// pins that routing through the resolver produces the same thing.
    #[test]
    fn proxy_row_of_merge_table_is_unchanged() {
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t",
                "proxy":{"request":{"remove":["Origin"],"set":{"X-A":"p","X-B":"p"}}},
                "nodes":{"a":{
                  "proxy":{"request":{"remove":["Referer"],"set":{"X-B":"n"}}},
                  "variants":{"dev":{"type":"start_server","shell":"x",
                    "proxy":{"request":{"remove":["origin"],"set":{"X-C":"v"}}}}}
                }}}"#,
            "a",
            "dev",
        );
        // remove: union, first spelling wins, case-insensitive dedup.
        assert_eq!(r.proxy.request.remove, vec!["Origin", "Referer"]);
        // set: per key, most specific wins.
        assert_eq!(r.proxy.request.set.get("X-A").unwrap(), "p");
        assert_eq!(r.proxy.request.set.get("X-B").unwrap(), "n");
        assert_eq!(r.proxy.request.set.get("X-C").unwrap(), "v");
    }

    /// F6: several named ports, and `${veld.port}` still means the primary.
    #[test]
    fn two_named_ports_both_declared_and_primary_resolves() {
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"start_server","shell":"x",
                "ports":{"http":"auto","debug":"auto"}
            }}}}}"#,
            "a",
            "dev",
        );
        let ports = r.ports.as_ref().unwrap();
        assert_eq!(ports.ports.len(), 2);
        assert_eq!(ports.primary, "http", "`http` is the primary by convention");

        // A single named port is the primary whatever it is called.
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"start_server","shell":"x","ports":{"grpc":"auto"}
            }}}}}"#,
            "a",
            "dev",
        );
        assert_eq!(r.ports.as_ref().unwrap().primary, "grpc");

        // No `ports` at all keeps the pre-F6 behaviour: one allocated port.
        let r = resolve(
            r#"{"schemaVersion":"3","name":"t","nodes":{"a":{"variants":{"dev":{
                "type":"start_server","shell":"x"
            }}}}}"#,
            "a",
            "dev",
        );
        assert!(r.ports.is_none());
    }

    #[test]
    fn port_spec_accepts_auto_and_fixed_and_rejects_nonsense() {
        assert_eq!(
            serde_json::from_str::<PortSpec>(r#""auto""#).unwrap(),
            PortSpec::Auto
        );
        assert_eq!(
            serde_json::from_str::<PortSpec>("5432").unwrap(),
            PortSpec::Fixed(5432)
        );
        assert!(serde_json::from_str::<PortSpec>("0").is_err());
        assert!(serde_json::from_str::<PortSpec>("70000").is_err());
        assert!(serde_json::from_str::<PortSpec>(r#""whatever""#).is_err());
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
            { "url": "https://cmd.example.com", "token": { "shell": "op read op://v/t" } }
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
                    token: Some(SecretSource::Shell("op read op://v/t".into())),
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
    fn test_share_policy_web_access_parses_and_defaults() {
        // Absent `web` → config silent → daemon applies password-by-default.
        let s: SharePolicy = serde_json::from_str(r#"{ "expose": ["web"] }"#).unwrap();
        assert_eq!(s.web_access(), None);
        // `"web": {}` is also silent (no explicit access chosen).
        let s: SharePolicy = serde_json::from_str(r#"{ "expose": ["web"], "web": {} }"#).unwrap();
        assert_eq!(s.web_access(), None);
        // Explicit values parse and are distinguishable from silence.
        let s: SharePolicy =
            serde_json::from_str(r#"{ "expose": ["web"], "web": { "access": "link" } }"#).unwrap();
        assert_eq!(s.web_access(), Some(WebAccessMode::Link));
        let s: SharePolicy =
            serde_json::from_str(r#"{ "expose": ["web"], "web": { "access": "password" } }"#)
                .unwrap();
        assert_eq!(s.web_access(), Some(WebAccessMode::Password));
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
            serde_json::from_str(r#"{ "type": "start_server", "shell": "x" }"#).unwrap();
        assert!(v.share.is_none());
    }

    #[test]
    fn test_sharing_config_parses_on_veld_config() {
        let json = r#"{
            "schemaVersion": "3",
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
                            "shell": "npm start",
                            "share": { "expose": ["peer"] }
                        }
                    }
                }
            }
        }"#;
        let cfg: VeldConfig = serde_json::from_str(json).unwrap();
        let sharing = cfg.sharing.clone().unwrap();
        assert_eq!(
            sharing.relays,
            Some(RelayPolicy::Custom(vec![RelayEntry::url(
                "https://relay.acme.internal"
            )]))
        );
        let gateway = sharing.gateway.expect("gateway parsed");
        assert_eq!(gateway.url, "https://share.acme.internal");
        assert_eq!(gateway.token, None);
        let share = cfg.resolved("web", "local").unwrap().share.unwrap();
        assert!(share.allows(ExposeMode::Peer));
        assert!(!share.allows(ExposeMode::Web));
    }

    #[test]
    fn gateway_ref_object_form_carries_token_and_round_trips() {
        // Object form with a token source.
        let gw: GatewayRef = serde_json::from_str(
            r#"{ "url": "https://share.acme.internal", "token": { "env": "GW_TOKEN" } }"#,
        )
        .unwrap();
        assert_eq!(gw.url, "https://share.acme.internal");
        assert_eq!(gw.token, Some(SecretSource::Env("GW_TOKEN".into())));

        // String shorthand round-trips as a bare string.
        let bare: GatewayRef = serde_json::from_str(r#""https://share.acme.internal""#).unwrap();
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            serde_json::json!("https://share.acme.internal")
        );

        // Unknown keys are rejected (typo protection, matching relay entries).
        assert!(
            serde_json::from_str::<GatewayRef>(r#"{ "url": "https://x", "tokn": "oops" }"#)
                .is_err()
        );
        // A literal token never appears in Debug output.
        let lit = GatewayRef {
            url: "https://x".into(),
            token: Some(SecretSource::Literal("s3cret".into())),
        };
        assert!(!format!("{lit:?}").contains("s3cret"));
    }
}
