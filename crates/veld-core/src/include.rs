//! Loading a config that is split across files (F2).
//!
//! One document type: **every** veld config file uses the same schema. All
//! top-level keys are optional except in the root file, which requires
//! `schemaVersion` and `name`. So `$schema` autocompletion works in every file,
//! and there is exactly one schema to learn.
//!
//! The rules that make this readable rather than a merge puzzle:
//!
//! - **A node is defined in exactly one file.** The same node name in two files
//!   is a hard error naming both. That is what removes precedence rules for node
//!   bodies entirely — there is never a question of which file won.
//! - **A file that fails to parse is a named error**, never a silently absent
//!   node. The failure mode this exists to prevent is "why is my node missing"
//!   answered three hours later by a typo in a file nobody looked at.
//! - **Relative paths stay relative to the project root**, not to the declaring
//!   file. Making them file-relative would silently change the meaning of every
//!   existing `cwd`, `script`, and output path.
//! - Duplicate `vars` or `preset` names across files are hard errors too. No
//!   shadowing, no file-local scope, no ordering dependency.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{
    ConfigError, ConfigValue, FeaturesConfig, NodeConfig, NullableMap, ProxyConfig, SetupStep,
    SharingConfig, VeldConfig,
};

/// One veld config file, as written.
///
/// Every field is optional: this is the shape of *any* config file, root or
/// included.
///
/// **Unknown top-level keys are captured, not rejected.** F8 wants a typo in a
/// top-level key to be an error — but `deny_unknown_fields` would make it a
/// *load* error, and the loader runs on `veld stop`, `status`, `logs`, and in the
/// daemon monitor (F0.1). Worse, it would be a **new** failure for v1 and v2
/// documents, which previously ignored an unknown key silently: a project with a
/// stray `"//": "comment"` (the pre-JSONC idiom) would upgrade into a config that
/// cannot be stopped, so its teardown hooks never run.
///
/// So unknown keys land in [`Self::unknown`] and become deferred findings —
/// `veld start` and `veld lint` refuse, everything else keeps working. Same
/// mechanism as duplicate keys; see [`crate::config::VeldConfig::deferred_findings`].
#[derive(Debug, Default, Deserialize)]
pub struct Document {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    #[serde(rename = "schemaVersion", default)]
    pub schema_version: Option<String>,
    #[serde(default)]
    pub name: Option<String>,

    /// Globs of further config files to load, relative to the root file's
    /// directory. Only meaningful in the root file.
    #[serde(default)]
    pub include: Option<Vec<String>>,

    #[serde(default)]
    pub url_template: Option<String>,
    #[serde(default)]
    pub presets: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub client_log_levels: Option<Vec<String>>,
    #[serde(default)]
    pub features: Option<FeaturesConfig>,
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
    #[serde(default)]
    pub env: Option<NullableMap<ConfigValue>>,
    #[serde(default)]
    pub vars: Option<HashMap<String, ConfigValue>>,
    #[serde(default)]
    pub sharing: Option<SharingConfig>,
    #[serde(default)]
    pub setup: Option<Vec<SetupStep>>,
    #[serde(default)]
    pub teardown: Option<Vec<SetupStep>>,
    #[serde(default)]
    pub nodes: Option<HashMap<String, NodeConfig>>,

    /// **Reserved, parsed and held, never executed** (F8). See
    /// [`crate::config::VeldConfig::hooks`].
    #[serde(default)]
    pub hooks: Option<serde_json::Value>,
    /// **Reserved, parsed and held, never executed** (F8).
    #[serde(default)]
    pub ui: Option<serde_json::Value>,

    /// Every top-level key that is not one of the above.
    ///
    /// Captured rather than rejected so a typo is still caught (as a finding) but
    /// cannot strand `veld stop` — see the type's doc comment.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// Where one loaded file came from, and what it contributed.
#[derive(Debug, Clone)]
pub struct LoadedFile {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Path relative to the project root — what gets shown to humans and hashed.
    pub relative: PathBuf,
    /// The include glob that matched it, or `None` for the root file.
    pub matched_by: Option<String>,
    /// Node names this file defines, with the line each is declared on.
    pub nodes: BTreeMap<String, usize>,
}

/// Everything a split load produced.
#[derive(Debug)]
pub struct LoadedConfig {
    pub config: VeldConfig,
    /// Every file that contributed, root first then glob matches in load order.
    pub files: Vec<LoadedFile>,
    /// The include globs from the root file, in declaration order.
    pub globs: Vec<String>,
    /// SHA-256 over sorted `(relative_path, bytes)` pairs across **every** loaded
    /// file.
    ///
    /// Hashing only the root file's bytes — as veld did when there was only one —
    /// would report "nothing changed" for almost every real change once the nodes
    /// live in other files, which is exactly the question `veld runs diff` exists
    /// to answer.
    pub config_hash: String,
}

/// Load a config, following `include` globs from the root file.
pub fn load(root_path: &Path) -> Result<LoadedConfig, ConfigError> {
    let project_root = crate::config::project_root(root_path);

    let root_text = read(root_path)?;
    let root_doc = parse_document(&root_text, root_path)?;

    // The two keys only the root file must have. Everything else is optional
    // everywhere, which is what makes it one document type.
    let schema_version =
        root_doc
            .schema_version
            .clone()
            .ok_or_else(|| ConfigError::MissingRootKey {
                path: root_path.to_path_buf(),
                key: "schemaVersion",
            })?;
    let name = root_doc
        .name
        .clone()
        .ok_or_else(|| ConfigError::MissingRootKey {
            path: root_path.to_path_buf(),
            key: "name",
        })?;
    if !crate::config::SUPPORTED_SCHEMA_VERSIONS.contains(&schema_version.as_str()) {
        return Err(ConfigError::UnsupportedSchemaVersion(schema_version));
    }

    let globs = root_doc.include.clone().unwrap_or_default();
    let mut hash_inputs: Vec<(PathBuf, Vec<u8>)> = vec![(
        relative_to(&project_root, root_path),
        root_text.clone().into_bytes(),
    )];

    let mut files = vec![LoadedFile {
        path: root_path.to_path_buf(),
        relative: relative_to(&project_root, root_path),
        matched_by: None,
        nodes: node_lines(&root_text, &root_doc),
    }];
    let mut docs: Vec<(usize, Document)> = vec![(0, root_doc)];

    // Problems in *included* files are reported, never fatal.
    //
    // F0.1 is absolute: no new failure may be reachable from the loader, because
    // `veld stop` reads `on_stop` from the on-disk config at stop time and a config
    // that will not load means teardown never runs. That applies to a broken
    // included file exactly as it applies to a duplicate key — a developer who
    // copies a node file while an environment is running must still be able to
    // stop it.
    //
    // "Never a silently absent node" is still honoured: the node is absent, but
    // loudly — `veld start` and `veld lint` refuse with a finding naming the file,
    // and `veld config --files` shows the gap. Silent was always the enemy, not
    // non-fatal.
    let mut file_findings: Vec<crate::config::Finding> = Vec::new();

    for glob in &globs {
        // Sorted, so error messages and load order are deterministic across
        // machines and filesystems.
        let mut matches = expand_glob(&project_root, glob);
        matches.sort();
        for path in matches {
            // The root file may also match its own glob (`*.json`); loading it
            // twice would report every node as a duplicate of itself.
            if path == root_path {
                continue;
            }
            let relative = relative_to(&project_root, &path);
            let text = match read(&path) {
                Ok(text) => text,
                Err(e) => {
                    file_findings.push(crate::config::Finding::unreadable_include(
                        &relative.display().to_string(),
                        &e.to_string(),
                    ));
                    continue;
                }
            };
            let mut doc = match parse_document(&text, &path) {
                Ok(doc) => doc,
                Err(e) => {
                    file_findings.push(crate::config::Finding::unparseable_include(
                        &relative.display().to_string(),
                        &e.to_string(),
                    ));
                    continue;
                }
            };
            if doc.include.take().is_some() {
                // Ignored rather than fatal, and reported so it is not silent.
                file_findings.push(crate::config::Finding::nested_include(
                    &relative.display().to_string(),
                ));
            }
            hash_inputs.push((relative.clone(), text.clone().into_bytes()));
            files.push(LoadedFile {
                relative,
                matched_by: Some(glob.clone()),
                nodes: node_lines(&text, &doc),
                path,
            });
            docs.push((files.len() - 1, doc));
        }
    }

    // Unknown top-level keys: an error, but reported rather than fatal. Collected
    // before `merge` consumes the documents.
    let unknown_findings: Vec<crate::config::Finding> = docs
        .iter()
        .flat_map(|(file_index, doc)| {
            let where_ = files[*file_index].relative.display().to_string();
            doc.unknown
                .keys()
                .map(move |key| crate::config::Finding::unknown_top_level_key(&where_, key))
        })
        .collect();

    let mut config = merge(schema_version, name, &docs, &files, &mut file_findings);
    config.loaded_from_multiple_files = files.len() > 1;
    config.deferred_findings.extend(unknown_findings);
    config.deferred_findings.extend(file_findings);

    Ok(LoadedConfig {
        config,
        config_hash: hash_files(&mut hash_inputs),
        files,
        globs,
    })
}

/// SHA-256 over sorted `(relative_path, bytes)` pairs.
fn hash_files(inputs: &mut [(PathBuf, Vec<u8>)]) -> String {
    use sha2::{Digest, Sha256};
    inputs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (path, bytes) in inputs.iter() {
        // Length-prefixed so `a/b` + `c` cannot hash the same as `a` + `b/c`.
        hasher.update((path.to_string_lossy().len() as u64).to_le_bytes());
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    format!("{:x}", hasher.finalize())
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

/// Parse one file: JSONC front end, then the shared document shape.
///
/// The legacy-command gate runs on every file unconditionally: only one schema
/// version is supported, so `command` is never valid, and an included file
/// legitimately omits `schemaVersion` — a per-file version check would have left
/// every included file free to keep the old form, which is exactly where the node
/// bodies live.
fn parse_document(text: &str, path: &Path) -> Result<Document, ConfigError> {
    let json = crate::jsonc::strip(text).map_err(|e| ConfigError::Jsonc {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| ConfigError::ParseError {
            path: path.to_path_buf(),
            source: e,
        })?;
    // The legacy-command gate, but **after** the version question is settled.
    //
    // A document that declares an unsupported `schemaVersion` must get the
    // "run `veld config --migrate`" error, not "`command` has been replaced" —
    // those are the same fix, and the version error is the one that explains it.
    // So skip the gate for a document whose own version is unsupported and let the
    // caller's version check produce the better message.
    //
    // A file with no `schemaVersion` is an included file: its project's root was
    // already version-checked, so the gate applies. Checking per file is what makes
    // the error name the file to edit, and is why this is not a per-file *version*
    // check — an included file legitimately omits the key, which would have left
    // every included file free to keep the old form.
    let own_version = value.get("schemaVersion").and_then(|v| v.as_str());
    let version_is_supported =
        own_version.is_none_or(|v| crate::config::SUPPORTED_SCHEMA_VERSIONS.contains(&v));
    if version_is_supported {
        crate::config::reject_v3_legacy_commands(&value, path)?;
    }
    // Deserialize from the **text**, not the already-parsed `Value`:
    // `serde_json::from_value` discards positions, so every typed error (`unknown
    // variant`, `invalid type`, a bad field) would report line 0 — losing exactly
    // the accuracy that stripping comments in place rather than deleting them
    // exists to preserve. The `Value` above is kept only for the v3 key gate.
    serde_json::from_str(&json).map_err(|e| ConfigError::ParseError {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Line number (1-based) of each node key in this file, for `veld nodes`.
///
/// Found by scanning the text for the quoted key rather than by tracking spans:
/// a node name is unique within its file (duplicates are rejected), and being off
/// by a line in a pathological case is a far smaller cost than a span-tracking
/// parser.
fn node_lines(text: &str, doc: &Document) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for name in doc.nodes.iter().flat_map(|n| n.keys()) {
        let needle = format!("\"{name}\"");
        let line = text
            .lines()
            .position(|l| l.contains(&needle))
            .map(|i| i + 1)
            .unwrap_or(0);
        out.insert(name.clone(), line);
    }
    out
}

/// Fold every document into one config, rejecting duplicates with provenance.
fn merge(
    schema_version: String,
    name: String,
    docs: &[(usize, Document)],
    files: &[LoadedFile],
    findings: &mut Vec<crate::config::Finding>,
) -> VeldConfig {
    let mut nodes: HashMap<String, NodeConfig> = HashMap::new();
    let mut node_origin: HashMap<String, usize> = HashMap::new();
    let mut presets: HashMap<String, Vec<String>> = HashMap::new();
    let mut preset_origin: HashMap<String, usize> = HashMap::new();
    let mut vars: HashMap<String, ConfigValue> = HashMap::new();
    let mut var_origin: HashMap<String, usize> = HashMap::new();
    let mut env: NullableMap<ConfigValue> = HashMap::new();
    let mut setup: Vec<SetupStep> = Vec::new();
    let mut teardown: Vec<SetupStep> = Vec::new();
    let mut hooks: Option<serde_json::Value> = None;
    let mut ui: Option<serde_json::Value> = None;

    // Project-level singletons come from the root file, which is docs[0].
    let root = &docs[0].1;

    for (file_index, doc) in docs {
        let here = |idx: &usize| files[*idx].relative.display().to_string();

        for (node_name, node) in doc.nodes.iter().flatten() {
            if let Some(previous) = node_origin.get(node_name) {
                // Both files named, because "which one wins" is not a question
                // this config system answers. It refuses — but as a *finding*, not
                // a load failure: F0.1 forbids a new stop-fatal error, and someone
                // who copies a node file while an environment is running still has
                // to be able to tear it down.
                findings.push(crate::config::Finding::duplicate_definition(
                    "node",
                    node_name,
                    &here(previous),
                    &here(file_index),
                ));
                continue;
            }
            node_origin.insert(node_name.clone(), *file_index);
            nodes.insert(node_name.clone(), node.clone());
        }

        for (preset_name, items) in doc.presets.iter().flatten() {
            if let Some(previous) = preset_origin.get(preset_name) {
                findings.push(crate::config::Finding::duplicate_definition(
                    "preset",
                    preset_name,
                    &here(previous),
                    &here(file_index),
                ));
                continue;
            }
            preset_origin.insert(preset_name.clone(), *file_index);
            presets.insert(preset_name.clone(), items.clone());
        }

        for (var_name, value) in doc.vars.iter().flatten() {
            if let Some(previous) = var_origin.get(var_name) {
                findings.push(crate::config::Finding::duplicate_definition(
                    "var",
                    var_name,
                    &here(previous),
                    &here(file_index),
                ));
                continue;
            }
            var_origin.insert(var_name.clone(), *file_index);
            vars.insert(var_name.clone(), value.clone());
        }

        // `env` is additive by key like the node cascade, so several files may
        // each contribute project-level variables.
        for (key, value) in doc.env.iter().flatten() {
            env.insert(key.clone(), value.clone());
        }
        setup.extend(doc.setup.iter().flatten().cloned());
        teardown.extend(doc.teardown.iter().flatten().cloned());
        if let Some(h) = &doc.hooks {
            merge_reserved(&mut hooks, h);
        }
        if let Some(u) = &doc.ui {
            merge_reserved(&mut ui, u);
        }
    }

    VeldConfig {
        schema: root.schema.clone(),
        schema_version,
        name,
        url_template: root
            .url_template
            .clone()
            .unwrap_or_else(crate::config::default_url_template),
        presets: (!presets.is_empty()).then_some(presets),
        client_log_levels: root.client_log_levels.clone(),
        features: root.features.clone(),
        proxy: root.proxy.clone(),
        env: (!env.is_empty()).then_some(env),
        vars: (!vars.is_empty()).then_some(vars),
        sharing: root.sharing.clone(),
        setup: (!setup.is_empty()).then_some(setup),
        teardown: (!teardown.is_empty()).then_some(teardown),
        nodes,
        hooks,
        ui,
        loaded_from_multiple_files: false,
        deferred_findings: Vec::new(),
    }
}

/// Shallow-merge two reserved (`hooks` / `ui`) objects by top-level key.
///
/// Reserved namespaces are opaque to this version, so the merge is deliberately
/// dumb: enough that several files can each contribute entries, with no attempt
/// to understand or deep-merge values veld does not interpret.
fn merge_reserved(into: &mut Option<serde_json::Value>, add: &serde_json::Value) {
    match (into.as_mut(), add) {
        (Some(serde_json::Value::Object(target)), serde_json::Value::Object(source)) => {
            for (k, v) in source {
                target.insert(k.clone(), v.clone());
            }
        }
        _ => *into = Some(add.clone()),
    }
}

// ---------------------------------------------------------------------------
// Globs
// ---------------------------------------------------------------------------

/// Expand one glob under `root`, returning absolute paths of matching files.
///
/// Hand-rolled rather than pulling in a glob crate: the patterns this needs to
/// support are `veld.d/*.jsonc` and `apps/*/veld.node.json`, the semantics below
/// are the whole surface, and the repo gates third-party licences in CI — a
/// dependency for sixty lines of segment matching is not worth that churn.
///
/// Semantics: `*` matches any run of characters **within one path segment**
/// (never `/`), `?` matches one character, `**` as a whole segment matches any
/// number of segments. No brace expansion, no character classes — if a config
/// needs those, list another glob.
fn expand_glob(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    walk(root, &segments, &mut out);
    out
}

fn walk(dir: &Path, segments: &[&str], out: &mut Vec<PathBuf>) {
    let Some((head, rest)) = segments.split_first() else {
        return;
    };

    if *head == "**" {
        // Match zero segments (try the rest right here) …
        walk(dir, rest, out);
        // … and one-or-more, by recursing into every subdirectory.
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    walk(&entry.path(), segments, out);
                }
            }
        }
        return;
    }

    // A literal segment needs no directory scan, which keeps the common
    // `veld.d/*.jsonc` case from reading directories it does not need.
    if !head.contains('*') && !head.contains('?') {
        let candidate = dir.join(head);
        if rest.is_empty() {
            if candidate.is_file() {
                out.push(candidate);
            }
        } else if candidate.is_dir() {
            walk(&candidate, rest, out);
        }
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !matches_segment(head, name) {
            continue;
        }
        let path = entry.path();
        if rest.is_empty() {
            if path.is_file() {
                out.push(path);
            }
        } else if path.is_dir() {
            walk(&path, rest, out);
        }
    }
}

/// Match one path segment against a pattern containing `*` and `?`.
fn matches_segment(pattern: &str, name: &str) -> bool {
    // A leading `.` is only matched by an explicit leading `.`, so `*.json` does
    // not pick up editor backup files or `.DS_Store`-style entries.
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    // Classic two-pointer wildcard match with backtracking on the last `*`.
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ni;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_matching() {
        assert!(matches_segment("*.jsonc", "vars.jsonc"));
        assert!(!matches_segment("*.jsonc", "vars.json"));
        assert!(matches_segment("veld.node.json", "veld.node.json"));
        assert!(matches_segment("*", "anything"));
        assert!(matches_segment("a?c", "abc"));
        assert!(!matches_segment("a?c", "ac"));
        assert!(matches_segment("*.node.*", "api.node.json"));
        // A dotfile is not swept up by a bare `*`.
        assert!(!matches_segment("*", ".DS_Store"));
        assert!(matches_segment(".*", ".hidden"));
    }

    /// `*` must not cross a path separator, or `apps/*/veld.node.json` would
    /// match arbitrarily deep and pull in files from unrelated trees.
    #[test]
    fn star_does_not_cross_segments() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("apps/web/nested")).unwrap();
        std::fs::write(root.join("apps/web/veld.node.json"), "{}").unwrap();
        std::fs::write(root.join("apps/web/nested/veld.node.json"), "{}").unwrap();

        let hits = expand_glob(root, "apps/*/veld.node.json");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0].ends_with("apps/web/veld.node.json"));

        // `**` is how you opt into crossing segments.
        let deep = expand_glob(root, "apps/**/veld.node.json");
        assert_eq!(deep.len(), 2, "{deep:?}");
    }

    #[test]
    fn hash_distinguishes_path_boundaries() {
        let mut a = vec![
            (PathBuf::from("a/b"), b"x".to_vec()),
            (PathBuf::from("c"), b"y".to_vec()),
        ];
        let mut b = vec![
            (PathBuf::from("a"), b"x".to_vec()),
            (PathBuf::from("b/c"), b"y".to_vec()),
        ];
        assert_ne!(hash_files(&mut a), hash_files(&mut b));
    }

    /// Write a project tree and return its root. `files` is `(relative path,
    /// contents)`; parent directories are created.
    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, contents).unwrap();
        }
        dir
    }

    const ROOT_WITH_INCLUDE: &str = r#"{
        "schemaVersion": "3",
        "name": "monorepo",
        "include": ["veld.d/*.jsonc", "services/*/veld.node.json"]
    }"#;

    fn node_file(name: &str) -> String {
        format!(
            r#"{{ "nodes": {{ "{name}": {{
                "type": "command",
                "shell": "true",
                "default_variant": "dev",
                "variants": {{ "dev": {{}} }}
            }} }} }}"#
        )
    }

    /// The load path a large monorepo actually uses: nodes in per-directory files,
    /// vars in a shared one, and every included file using the same schema with all
    /// top-level keys optional.
    #[test]
    fn included_files_contribute_nodes_vars_and_presets() {
        let dir = project(&[
            ("veld.json", ROOT_WITH_INCLUDE),
            (
                "veld.d/vars.jsonc",
                r#"{
                    // Shared values, one definition point each.
                    "vars": { "remote_api": "https://api.example.com" },
                    "presets": { "core": ["api:dev"] },
                }"#,
            ),
            ("services/api/veld.node.json", &node_file("api")),
            ("services/worker/veld.node.json", &node_file("worker")),
        ]);

        let loaded = load(&dir.path().join("veld.json")).expect("split config loads");
        assert_eq!(loaded.config.name, "monorepo");
        let mut names: Vec<&str> = loaded.config.nodes.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names, vec!["api", "worker"]);
        assert!(
            loaded
                .config
                .vars
                .as_ref()
                .unwrap()
                .contains_key("remote_api")
        );
        assert!(
            loaded.config.presets.as_ref().unwrap().contains_key("core"),
            "a preset may live in any file"
        );

        // Provenance: which file defined what, and which glob found it.
        assert_eq!(loaded.files.len(), 4, "root + three included");
        let api = loaded
            .files
            .iter()
            .find(|f| f.nodes.contains_key("api"))
            .expect("api's file is recorded");
        assert_eq!(
            api.relative,
            PathBuf::from("services/api/veld.node.json"),
            "so `veld nodes` can say where a node is defined"
        );
        assert_eq!(api.matched_by.as_deref(), Some("services/*/veld.node.json"));
        assert!(api.nodes["api"] > 0, "and on which line");
    }

    /// The rule that removes precedence entirely: two files, one node name, both
    /// files named.
    ///
    /// Reported, **not fatal** — F0.1 again. Someone who copies a node file while
    /// an environment is running must still be able to `veld stop` it, so this is a
    /// finding that blocks `start`/`lint` rather than a load error.
    #[test]
    fn duplicate_node_across_files_names_both_files() {
        let dir = project(&[
            ("veld.json", ROOT_WITH_INCLUDE),
            ("services/a/veld.node.json", &node_file("api")),
            ("services/b/veld.node.json", &node_file("api")),
        ]);
        let loaded = load(&dir.path().join("veld.json"))
            .expect("a duplicate node must not strand `veld stop`");
        let finding = loaded
            .config
            .deferred_findings
            .iter()
            .find(|f| f.rule == "duplicate-definition")
            .unwrap_or_else(|| panic!("expected a finding: {:?}", loaded.config.deferred_findings));
        assert_eq!(finding.severity, crate::config::Severity::Error);
        let msg = &finding.message;
        assert!(msg.contains("services/a/veld.node.json"), "{msg}");
        assert!(msg.contains("services/b/veld.node.json"), "{msg}");
        assert!(msg.contains("\"api\""), "{msg}");
        // …and it blocks a run.
        assert!(crate::config::error_summary(&crate::config::validate(&loaded.config)).is_some());
    }

    #[test]
    fn duplicate_vars_and_presets_across_files_are_errors() {
        for (kind, body) in [
            ("var", r#"{ "vars": { "shared": "x" } }"#),
            ("preset", r#"{ "presets": { "core": ["api:dev"] } }"#),
        ] {
            let dir = project(&[
                ("veld.json", ROOT_WITH_INCLUDE),
                ("veld.d/one.jsonc", body),
                ("veld.d/two.jsonc", body),
            ]);
            let loaded = load(&dir.path().join("veld.json")).expect("reported, not fatal");
            let finding = loaded
                .config
                .deferred_findings
                .iter()
                .find(|f| f.rule == "duplicate-definition")
                .unwrap_or_else(|| panic!("{kind}: expected a finding"));
            let msg = &finding.message;
            assert!(msg.contains(kind), "{msg}");
            assert!(msg.contains("veld.d/one.jsonc"), "{msg}");
            assert!(msg.contains("veld.d/two.jsonc"), "{msg}");
        }
    }

    /// The worst diagnostic this feature could produce is "unknown node" for a file
    /// with a typo in it. A broken included file is a named, fatal error.
    #[test]
    fn unparseable_included_file_is_named_error() {
        let dir = project(&[
            ("veld.json", ROOT_WITH_INCLUDE),
            ("services/api/veld.node.json", &node_file("api")),
            ("services/broken/veld.node.json", "{ \"nodes\": { oops }"),
        ]);
        let loaded = load(&dir.path().join("veld.json"))
            .expect("a broken included file must not strand `veld stop`");
        let finding = loaded
            .config
            .deferred_findings
            .iter()
            .find(|f| f.rule == "unparseable-include")
            .unwrap_or_else(|| panic!("expected a finding: {:?}", loaded.config.deferred_findings));
        assert_eq!(finding.severity, crate::config::Severity::Error);
        assert_eq!(finding.location, "services/broken/veld.node.json");
        // Not silent, which was always the real requirement: `veld start` refuses.
        assert!(crate::config::error_summary(&crate::config::validate(&loaded.config)).is_some());
        // The healthy sibling still loaded, so `stop` can still find its node.
        assert!(loaded.config.nodes.contains_key("api"));
    }

    /// An unknown top-level key is an error — but a **reported** one, not a load
    /// failure (F8 rule 2 reconciled with F0.1).
    ///
    /// `deny_unknown_fields` would have been the obvious implementation and is
    /// wrong twice over: it puts a new failure on the loader that `veld stop`
    /// uses, and it is a *regression for v1/v2 documents*, which previously
    /// ignored an unknown key silently. A project with a stray key would upgrade
    /// into a config that cannot be stopped, so its teardown never runs.
    #[test]
    fn unknown_top_level_key_is_reported_not_fatal() {
        let dir = project(&[
            ("veld.json", ROOT_WITH_INCLUDE),
            ("services/api/veld.node.json", &node_file("api")),
            ("veld.d/typo.jsonc", r#"{ "noeds": {} }"#),
        ]);
        let loaded =
            load(&dir.path().join("veld.json")).expect("an unknown key must not fail the load");

        // The rest of the config is intact — the typo did not eat the run.
        assert!(loaded.config.nodes.contains_key("api"));

        let finding = loaded
            .config
            .deferred_findings
            .iter()
            .find(|f| f.rule == "unknown-top-level-key")
            .unwrap_or_else(|| {
                panic!(
                    "expected a finding, got {:?}",
                    loaded.config.deferred_findings
                )
            });
        assert_eq!(finding.severity, crate::config::Severity::Error);
        assert!(finding.message.contains("\"noeds\""), "{finding:?}");
        // Names the file, so a typo in one of twenty included files is findable.
        assert_eq!(finding.location, "veld.d/typo.jsonc");
        // Lists the real keys, since the cause is a typo.
        assert!(finding.message.contains("nodes"), "{finding:?}");

        // …and `validate` surfaces it, so `veld start` / `veld lint` refuse.
        let findings = crate::config::validate(&loaded.config);
        assert!(findings.iter().any(|f| f.rule == "unknown-top-level-key"));
        assert!(crate::config::error_summary(&findings).is_some());
    }

    /// The regression that made the above necessary: a v1/v2 config carrying the
    /// pre-JSONC `"//"` comment idiom must still load, and the diagnostic must say
    /// that real comments now exist.
    #[test]
    fn legacy_comment_key_still_loads_and_suggests_a_real_comment() {
        let dir = project(&[(
            "veld.json",
            r#"{
                "//": "a comment, the pre-JSONC way",
                "schemaVersion": "3",
                "name": "legacy",
                "nodes": { "api": { "default_variant": "dev", "variants": { "dev": {
                    "type": "command", "shell": "true"
                } } } }
            }"#,
        )]);
        let loaded = load(&dir.path().join("veld.json"))
            .expect("a v1/v2 config with a stray key must keep loading");
        assert!(loaded.config.nodes.contains_key("api"));

        let finding = loaded
            .config
            .deferred_findings
            .iter()
            .find(|f| f.rule == "unknown-top-level-key")
            .expect("still reported");
        assert!(
            finding.message.contains("`//` comments"),
            "must point at the replacement: {finding:?}"
        );
    }

    /// `KNOWN_TOP_LEVEL_KEYS` powers the unknown-key error message, so a list that
    /// drifts from the real fields turns a helpful diagnostic into a misleading
    /// one — it would name a key as valid that serde rejects, or omit a valid one.
    #[test]
    fn known_top_level_keys_matches_document() {
        // Every advertised key must actually deserialize into a field rather than
        // landing in `unknown`.
        for key in crate::config::KNOWN_TOP_LEVEL_KEYS {
            let value = match *key {
                "schemaVersion" | "name" | "url_template" | "$schema" => "\"x\"".to_owned(),
                "include" | "client_log_levels" => "[]".to_owned(),
                "setup" | "teardown" => "[]".to_owned(),
                _ => "{}".to_owned(),
            };
            let doc: Document =
                serde_json::from_str(&format!("{{ {} : {value} }}", format_args!("\"{key}\"")))
                    .unwrap_or_else(|e| panic!("{key} should be a known field: {e}"));
            assert!(
                doc.unknown.is_empty(),
                "{key} is advertised as known but landed in `unknown`"
            );
        }
    }

    /// `hooks` and `ui` are reserved, so they parse anywhere — and are not
    /// executed (F8).
    #[test]
    fn reserved_namespaces_parse_and_round_trip() {
        let dir = project(&[
            ("veld.json", ROOT_WITH_INCLUDE),
            (
                "veld.d/hooks.jsonc",
                r#"{ "hooks": { "worktree.created": [ { "argv": ["./setup.sh"] } ] } }"#,
            ),
            (
                "veld.d/ui.jsonc",
                r#"{ "ui": { "my-ext": { "title": "Mine", "panel": "p" } } }"#,
            ),
        ]);
        let loaded = load(&dir.path().join("veld.json")).unwrap();
        let hooks = loaded.config.hooks.as_ref().expect("hooks are held");
        assert!(hooks.get("worktree.created").is_some());
        assert!(loaded.config.ui.as_ref().unwrap().get("my-ext").is_some());
    }

    /// `config_hash` must change when **any** loaded file changes, or
    /// `veld runs diff` reports "nothing changed" for almost every real edit once
    /// the nodes live outside the root file.
    #[test]
    fn config_hash_changes_on_any_included_file() {
        let dir = project(&[
            ("veld.json", ROOT_WITH_INCLUDE),
            ("services/api/veld.node.json", &node_file("api")),
        ]);
        let root = dir.path().join("veld.json");
        let before = load(&root).unwrap().config_hash;

        // Edit an *included* file, not the root.
        std::fs::write(
            dir.path().join("services/api/veld.node.json"),
            node_file("api").replace("true", "echo changed"),
        )
        .unwrap();
        let after = load(&root).unwrap().config_hash;
        assert_ne!(before, after, "an included-file edit must change the hash");

        // Adding a file changes it too.
        std::fs::create_dir_all(dir.path().join("services/worker")).unwrap();
        std::fs::write(
            dir.path().join("services/worker/veld.node.json"),
            node_file("worker"),
        )
        .unwrap();
        assert_ne!(after, load(&root).unwrap().config_hash);
    }

    /// Relative paths stay relative to the **project root**, not to the file that
    /// declares them. Making them file-relative would silently change the meaning
    /// of every existing `cwd`, `script`, and output path.
    #[test]
    fn relative_paths_resolve_from_project_root() {
        let dir = project(&[
            ("veld.json", ROOT_WITH_INCLUDE),
            (
                "services/api/veld.node.json",
                r#"{ "nodes": { "api": {
                    "type": "command",
                    "cwd": "services/api",
                    "default_variant": "dev",
                    "variants": { "dev": { "script": "scripts/build.sh" } }
                } } }"#,
            ),
        ]);
        let root = dir.path().join("veld.json");
        let loaded = load(&root).unwrap();
        let node = &loaded.config.nodes["api"];

        // Written in services/api/veld.node.json, but still resolved from the root.
        assert_eq!(
            crate::config::resolve_cwd(dir.path(), node.cwd.as_deref(), None),
            dir.path().join("services/api"),
            "a file-relative reading would give services/api/services/api"
        );
        let resolved = loaded.config.resolved("api", "dev").unwrap();
        assert_eq!(
            resolved.script.as_deref(),
            Some("scripts/build.sh"),
            "the script path is stored verbatim and joined to the project root at spawn"
        );
    }

    /// Only the root file may declare the two required keys' absence — and only
    /// the root file may `include`.
    #[test]
    fn root_requires_two_keys_and_owns_include() {
        let dir = project(&[("veld.json", r#"{ "name": "no-version" }"#)]);
        assert!(matches!(
            load(&dir.path().join("veld.json")),
            Err(ConfigError::MissingRootKey {
                key: "schemaVersion",
                ..
            })
        ));

        let dir = project(&[("veld.json", r#"{ "schemaVersion": "3" }"#)]);
        assert!(matches!(
            load(&dir.path().join("veld.json")),
            Err(ConfigError::MissingRootKey { key: "name", .. })
        ));
    }

    /// A single-file config — every config that exists today — keeps loading with
    /// no `include` at all.
    #[test]
    fn single_file_config_still_loads() {
        let dir = project(&[(
            "veld.json",
            r#"{
                "schemaVersion": "3",
                "name": "classic",
                "nodes": { "api": { "default_variant": "dev", "variants": { "dev": {
                    "type": "command", "shell": "echo hi"
                } } } }
            }"#,
        )]);
        let loaded = load(&dir.path().join("veld.json")).unwrap();
        assert_eq!(loaded.files.len(), 1);
        assert!(loaded.globs.is_empty());
        assert_eq!(
            loaded.config.resolved("api", "dev").unwrap().command,
            Some(crate::config::CommandSpec::Shell("echo hi".into()))
        );
    }

    /// A glob that matches the root file must not load it twice — every node would
    /// then be reported as a duplicate of itself.
    #[test]
    fn root_file_matched_by_its_own_glob_is_not_loaded_twice() {
        let dir = project(&[(
            "veld.json",
            r#"{ "schemaVersion": "3", "name": "self", "include": ["*.json"],
                 "nodes": { "api": { "type": "command", "shell": "true",
                   "default_variant": "dev", "variants": { "dev": {} } } } }"#,
        )]);
        let loaded = load(&dir.path().join("veld.json")).expect("must not self-duplicate");
        assert_eq!(loaded.files.len(), 1);
    }

    #[test]
    fn hash_is_order_independent() {
        let mut a = vec![
            (PathBuf::from("z"), b"1".to_vec()),
            (PathBuf::from("a"), b"2".to_vec()),
        ];
        let mut b = vec![
            (PathBuf::from("a"), b"2".to_vec()),
            (PathBuf::from("z"), b"1".to_vec()),
        ];
        assert_eq!(hash_files(&mut a), hash_files(&mut b));
    }
}
