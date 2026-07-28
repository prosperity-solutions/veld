//! Converting a v1/v2 config to `schemaVersion: "3"`.
//!
//! **Dry-run by default.** Tokenizing a shell string into an argv is a heuristic,
//! so this never writes without showing the diff first, and anything ambiguous is
//! left as `shell` rather than guessed at. A wrong guess here would change what a
//! command *does* — `sh -c "a | b"` and `["a", "|", "b"]` are not the same program
//! — and the whole point of `shell` remaining first-class is that leaving it alone
//! is always a correct answer.
//!
//! Operates on the JSON text, not on the parsed model: a round-trip through serde
//! would reformat the whole file and delete every comment, which is precisely what
//! the JSONC support exists to allow. So the rewrite is textual and surgical.

use std::path::Path;

use crate::config::ConfigError;

/// What a migration would change.
#[derive(Debug, Default)]
pub struct Migration {
    /// The rewritten document text.
    pub migrated: String,
    /// One line per change, for the human to read before agreeing to it.
    pub changes: Vec<String>,
    /// Places the author has to look at, because the conversion was not safe to
    /// do automatically.
    pub manual: Vec<String>,
}

impl Migration {
    pub fn is_noop(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Plan the migration of one config file.
pub fn plan(path: &Path) -> Result<Migration, ConfigError> {
    let original = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(plan_text(&original))
}

/// [`plan`] over text, so it is testable without a file.
pub fn plan_text(original: &str) -> Migration {
    let mut out = Migration {
        migrated: original.to_owned(),
        ..Default::default()
    };

    // 1. The version itself.
    if let Some(rewritten) = bump_schema_version(&out.migrated) {
        out.migrated = rewritten;
        out.changes.push("schemaVersion → \"3\"".to_owned());
    }

    // 2. `command` → `argv` / `shell`, and bare-string `on_stop` / `skip_if` →
    //    `{ "shell": … }`.
    let (rewritten, converted, manual) = convert_commands(&out.migrated);
    out.migrated = rewritten;
    out.changes.extend(converted);
    out.manual.extend(manual);

    out
}

/// Replace the `schemaVersion` value with `"3"`, preserving surrounding
/// formatting.
fn bump_schema_version(text: &str) -> Option<String> {
    let key = "\"schemaVersion\"";
    let key_at = text.find(key)?;
    let after = &text[key_at + key.len()..];
    let colon = after.find(':')?;
    let rest = &after[colon + 1..];
    // The existing value, as written.
    let start = rest.find('"')?;
    let end = rest[start + 1..].find('"')? + start + 1;
    let current = &rest[start + 1..end];
    if current == "3" {
        return None;
    }
    let value_at = key_at + key.len() + colon + 1 + start;
    let mut migrated = String::with_capacity(text.len());
    migrated.push_str(&text[..value_at]);
    migrated.push_str("\"3\"");
    migrated.push_str(&text[value_at + (end - start) + 1..]);
    Some(migrated)
}

/// Rewrite every `"command": "…"` and every bare-string `on_stop` / `skip_if`.
///
/// Returns the new text, a description per change, and anything left for a human.
fn convert_commands(text: &str) -> (String, Vec<String>, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut changes = Vec::new();
    let mut manual = Vec::new();
    let mut rest = text;

    // Keys whose *string* value becomes an { argv | shell } object rather than a
    // sibling key.
    const NESTED_KEYS: [&str; 2] = ["on_stop", "skip_if"];

    // Find the next occurrence that is genuinely a *key*.
    //
    // `"command"` also appears as a **value** — `"type": "command"` is the
    // commonest line in any veld config — so a bare substring search would mangle
    // it into `"type": "argv": [...]`. A key is followed, after whitespace, by a
    // colon.
    fn next_key(rest: &str) -> Option<(usize, &'static str)> {
        ["command", "on_stop", "skip_if", "verify"]
            .iter()
            .filter_map(|k| {
                let needle = format!("\"{k}\"");
                let mut from = 0;
                while let Some(i) = rest[from..].find(&needle) {
                    let at = from + i;
                    if rest[at + needle.len()..].trim_start().starts_with(':') {
                        return Some((at, *k));
                    }
                    from = at + needle.len();
                }
                None
            })
            .min_by_key(|(i, _)| *i)
    }

    while let Some((offset, key)) = next_key(rest) {
        let needle_len = key.len() + 2;
        // Everything up to and including the key name is copied verbatim.
        out.push_str(&rest[..offset]);
        let after_key = &rest[offset + needle_len..];

        // The value: skip whitespace after the colon.
        let Some(colon) = after_key.find(':') else {
            out.push_str(&rest[offset..offset + needle_len]);
            rest = after_key;
            continue;
        };
        let value_start_rel = colon + 1;
        let ws: usize = after_key[value_start_rel..]
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(char::len_utf8)
            .sum();
        let value_at = value_start_rel + ws;

        // Only a string value is convertible; an object value is already v3.
        let Some(literal) = read_json_string(&after_key[value_at..]) else {
            out.push_str(&rest[offset..offset + needle_len]);
            rest = after_key;
            continue;
        };

        let (decoded, raw_len) = literal;
        let replacement = if NESTED_KEYS.contains(&key) || key == "verify" {
            // A nested command carrier: the *value* becomes the object.
            let key_out = if key == "verify" { "skip_if" } else { key };
            match tokenize(&decoded) {
                Some(argv) => {
                    changes.push(format!("{key_out}: string → {{ \"argv\": {argv:?} }}"));
                    format!(
                        "\"{key_out}\"{}: {{ \"argv\": {} }}",
                        "",
                        serde_json::to_string(&argv).unwrap()
                    )
                }
                None => {
                    changes.push(format!("{key_out}: string → {{ \"shell\": … }}"));
                    format!(
                        "\"{key_out}\": {{ \"shell\": {} }}",
                        serde_json::to_string(&decoded).unwrap()
                    )
                }
            }
        } else {
            // A sibling key: `"command": "x"` becomes `"argv": [...]` or
            // `"shell": "x"`.
            match tokenize(&decoded) {
                Some(argv) => {
                    changes.push(format!("command → argv {argv:?}"));
                    format!("\"argv\": {}", serde_json::to_string(&argv).unwrap())
                }
                None => {
                    changes.push(format!(
                        "command → shell (not safely tokenizable): {}",
                        one_line(&decoded)
                    ));
                    manual.push(format!(
                        "left as `shell` because it uses shell syntax: {}",
                        one_line(&decoded)
                    ));
                    format!("\"shell\": {}", serde_json::to_string(&decoded).unwrap())
                }
            }
        };

        out.push_str(&replacement);
        rest = &after_key[value_at + raw_len..];
    }
    out.push_str(rest);
    (out, changes, manual)
}

/// Read a JSON string literal at the start of `s`, returning its decoded value
/// and how many bytes it occupied.
fn read_json_string(s: &str) -> Option<(String, usize)> {
    if !s.starts_with('"') {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 1;
    let mut escaped = false;
    while i < bytes.len() {
        if escaped {
            escaped = false;
        } else if bytes[i] == b'\\' {
            escaped = true;
        } else if bytes[i] == b'"' {
            let raw = &s[..=i];
            let decoded: String = serde_json::from_str(raw).ok()?;
            return Some((decoded, raw.len()));
        }
        i += 1;
    }
    None
}

/// Split a shell string into an argv, or `None` when that would change meaning.
///
/// Conservative by design: the moment a character could mean something to a
/// shell, the answer is `None` and the command stays a `shell` string. That is
/// always correct, whereas a wrong split silently changes the program that runs.
///
/// Note that `${veld.port}` is *veld's* interpolation, not the shell's, so `$`
/// followed by `{` is not disqualifying on its own — but a bare `$VAR` is, since
/// only a shell expands that.
fn tokenize(command: &str) -> Option<Vec<String>> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    // Tracks whether we are inside a `${…}` reference, which is veld's own
    // interpolation rather than the shell's and therefore fine inside an argv
    // element — its braces must not be mistaken for brace expansion.
    let mut in_reference = false;
    for (i, c) in chars.iter().enumerate() {
        if in_reference {
            if *c == '}' {
                in_reference = false;
            }
            continue;
        }
        match c {
            '$' => {
                // `${…}` survives into an element; a bare `$VAR` is a shell
                // expansion and only a shell can perform it.
                if chars.get(i + 1) == Some(&'{') {
                    in_reference = true;
                } else {
                    return None;
                }
            }
            // Redirection, pipes, sequencing, subshells, globs, quoting,
            // backgrounding, comments, home expansion, brace expansion,
            // escapes, and `VAR=value` prefixes.
            '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`' | '"' | '\'' | '*' | '?' | '[' | ']'
            | '{' | '}' | '~' | '#' | '\\' | '\n' | '\r' | '=' => return None,
            _ => {}
        }
    }
    if in_reference {
        // An unclosed `${` is not something to guess about.
        return None;
    }
    let argv: Vec<String> = trimmed.split_whitespace().map(str::to_owned).collect();
    (!argv.is_empty()).then_some(argv)
}

/// Collapse a command to one line for a diff summary.
fn one_line(s: &str) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() > 60 {
        format!("{}…", flat.chars().take(60).collect::<String>())
    } else {
        flat
    }
}

/// A minimal unified diff between two texts, for the dry run.
///
/// Line-level and context-free beyond three lines: the point is for a human to see
/// what changed before agreeing to it, not to feed a patch tool.
pub fn unified_diff(original: &str, migrated: &str) -> String {
    let a: Vec<&str> = original.lines().collect();
    let b: Vec<&str> = migrated.lines().collect();
    let mut out = String::new();
    // The rewrite is line-preserving (it only substitutes within lines), so a
    // positional comparison is exact here and avoids an LCS implementation.
    if a.len() == b.len() {
        for (i, (old, new)) in a.iter().zip(b.iter()).enumerate() {
            if old != new {
                out.push_str(&format!("@@ line {} @@\n-{old}\n+{new}\n", i + 1));
            }
        }
        return out;
    }
    // Fallback for a non-line-preserving rewrite: show both wholesale rather than
    // pretending to be a diff tool.
    out.push_str("--- before\n");
    for line in &a {
        out.push_str(&format!("-{line}\n"));
    }
    out.push_str("+++ after\n");
    for line in &b {
        out.push_str(&format!("+{line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_only_what_is_safe() {
        // Plain commands become argv.
        assert_eq!(
            tokenize("pnpm dev"),
            Some(vec!["pnpm".to_owned(), "dev".to_owned()])
        );
        assert_eq!(
            tokenize("  node   server.js  "),
            Some(vec!["node".to_owned(), "server.js".to_owned()])
        );
        // veld's own interpolation survives into an element.
        assert_eq!(
            tokenize("next dev --port ${veld.port}"),
            Some(vec![
                "next".to_owned(),
                "dev".to_owned(),
                "--port".to_owned(),
                "${veld.port}".to_owned()
            ])
        );

        // Anything a shell would interpret stays a shell string.
        for shellish in [
            "a | b",
            "a && b",
            "a; b",
            "a > out.log",
            "a 2>&1",
            "echo $HOME",
            "ls *.txt",
            "echo \"quoted\"",
            "echo 'quoted'",
            "cd ~/x && y",
            "a $(b)",
            "a `b`",
            "FOO=bar cmd",
            "a &",
            "a # comment",
            "printf 'x\\n'",
            "",
            "   ",
        ] {
            assert_eq!(tokenize(shellish), None, "{shellish:?} must stay shell");
        }
    }

    #[test]
    fn bumps_the_version_in_place() {
        let src = "{\n  \"schemaVersion\": \"2\",\n  \"name\": \"x\"\n}";
        let m = plan_text(src);
        assert!(m.migrated.contains("\"schemaVersion\": \"3\""));
        assert!(m.migrated.contains("\"name\": \"x\""));
        assert!(m.changes.iter().any(|c| c.contains("schemaVersion")));

        // Already v3: nothing to do.
        assert!(plan_text(&m.migrated).is_noop());
    }

    #[test]
    fn converts_command_to_argv_or_shell() {
        let src = r#"{
  "schemaVersion": "2",
  "name": "x",
  "nodes": {
    "api": { "variants": { "dev": {
      "type": "start_server",
      "command": "pnpm dev",
      "on_stop": "docker rm -f api"
    } } },
    "logs": { "variants": { "dev": {
      "type": "command",
      "command": "tail -f x | grep y"
    } } }
  }
}"#;
        let m = plan_text(src);
        // Safely tokenizable → argv.
        assert!(
            m.migrated.contains(r#""argv": ["pnpm","dev"]"#),
            "{}",
            m.migrated
        );
        // A pipeline is not → stays shell, and is flagged for a human.
        assert!(
            m.migrated.contains(r#""shell": "tail -f x | grep y""#),
            "{}",
            m.migrated
        );
        assert!(m.manual.iter().any(|s| s.contains("tail -f x")));
        // A bare-string `on_stop` becomes the object form.
        assert!(
            m.migrated
                .contains(r#""on_stop": { "argv": ["docker","rm","-f","api"] }"#),
            "{}",
            m.migrated
        );

        // The result parses as v3 — the real test of a migration.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("veld.json");
        std::fs::write(&path, &m.migrated).unwrap();
        let cfg = crate::config::parse_config(&path)
            .unwrap_or_else(|e| panic!("migrated config must load as v3: {e}\n{}", m.migrated));
        assert_eq!(cfg.schema_version, "3");
    }

    /// The migration must preserve what the config *does*: the resolved commands
    /// before and after have to match.
    #[test]
    fn migrate_v2_preserves_effective_values() {
        let src = r#"{
  "schemaVersion": "2",
  "name": "x",
  "env": { "REGION": "eu-central-1" },
  "nodes": {
    "api": { "default_variant": "dev", "variants": { "dev": {
      "type": "start_server",
      "command": "node server.js",
      "health_check": { "type": "port" },
      "env": { "PORT": "${veld.port}" },
      "on_stop": "docker rm -f api"
    } } }
  }
}"#;
        // The v2 original can no longer be loaded — v2 is not a supported version,
        // which is the whole reason `--migrate` exists. So this asserts the
        // *migrated* config resolves to the values the v2 text described, rather
        // than diffing two loaded configs.
        let dir = tempfile::tempdir().unwrap();
        let m = plan_text(src);
        let after_path = dir.path().join("after/veld.json");
        std::fs::create_dir_all(after_path.parent().unwrap()).unwrap();
        std::fs::write(&after_path, &m.migrated).unwrap();
        let after = crate::config::parse_config(&after_path).unwrap();
        let after_resolved = after.resolved("api", "dev").unwrap();

        // The command means the same thing, expressed the safer way.
        assert_eq!(
            after_resolved.command,
            Some(crate::config::CommandSpec::Argv(vec![
                "node".into(),
                "server.js".into()
            ]))
        );
        // Everything the v2 text described survives, unchanged in meaning.
        assert_eq!(
            after_resolved.step_type,
            crate::config::StepType::StartServer
        );
        assert_eq!(after_resolved.env.as_ref().map(|e| e.len()), Some(2));
        assert_eq!(
            after_resolved
                .readiness
                .as_ref()
                .map(|p| p.check_type.as_str()),
            Some("port")
        );
        // `on_stop` is converted too — same words, safer form.
        assert_eq!(
            after_resolved.on_stop,
            Some(crate::config::CommandSpec::Argv(vec![
                "docker".into(),
                "rm".into(),
                "-f".into(),
                "api".into()
            ]))
        );
    }

    /// Comments and formatting survive, which is the reason this is a textual
    /// rewrite rather than a serde round-trip.
    #[test]
    fn comments_and_formatting_survive() {
        let src = r#"{
  // The API service. Owned by @some-team.
  "schemaVersion": "2",
  "name": "x",
  "nodes": {
    "api": { "variants": { "dev": {
      "type": "start_server",
      /* block comment */
      "command": "pnpm dev"
    } } }
  }
}"#;
        let m = plan_text(src);
        assert!(
            m.migrated
                .contains("// The API service. Owned by @some-team.")
        );
        assert!(m.migrated.contains("/* block comment */"));
        assert!(m.migrated.contains("\"argv\""));
    }

    #[test]
    fn diff_shows_only_changed_lines() {
        let src = "{\n  \"schemaVersion\": \"2\",\n  \"name\": \"x\"\n}";
        let m = plan_text(src);
        let diff = unified_diff(src, &m.migrated);
        assert!(diff.contains("-  \"schemaVersion\": \"2\","), "{diff}");
        assert!(diff.contains("+  \"schemaVersion\": \"3\","), "{diff}");
        assert!(
            !diff.contains("\"name\""),
            "unchanged lines stay out: {diff}"
        );
    }

    /// An object-valued `on_stop` is already v3 and must be left alone rather than
    /// wrapped a second time.
    #[test]
    fn already_migrated_values_are_untouched() {
        let src = r#"{
  "schemaVersion": "3",
  "name": "x",
  "nodes": { "api": { "variants": { "dev": {
    "type": "command",
    "argv": ["pnpm", "dev"],
    "on_stop": { "shell": "docker rm -f api" }
  } } } }
}"#;
        let m = plan_text(src);
        assert!(m.is_noop(), "changes: {:?}", m.changes);
        assert_eq!(m.migrated, src);
    }
}
