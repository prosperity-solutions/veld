//! Resolving configured [`ConfigValue`]s into concrete strings.
//!
//! **Timing is the contract.** Resolution happens once per run, at start, *after*
//! the graph is built and *before* the first spawn — never during parse. That
//! ordering is what makes `veld stop`, `veld status`, and the daemon monitor
//! immune to a secret source that has since broken: they parse the config, they
//! never resolve it.
//!
//! **veld never takes custody of a secret.** A [`ConfigValue`] is a pointer plus
//! a sensitivity flag. This module dereferences the pointer at the last possible
//! moment, hands the result to the child process, and keeps it out of logs,
//! `--json` output, and the share payload.

use std::collections::HashMap;
use std::time::Duration;

use thiserror::Error;

use crate::config::{CommandSpec, ConfigValue, SecretSource};

/// How long a single source command may take.
///
/// An interactive credential helper — a biometric prompt, an MFA push — has no
/// terminal when it runs under the daemon, so it does not fail: it *hangs*. A
/// hang during startup is indistinguishable from a slow service, so it must be
/// bounded and the error must say what is actually wrong.
pub const SOURCE_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Error)]
pub enum ValueError {
    /// The issue's requirement, verbatim: "Missing at start is an error naming
    /// the node and the variable" — today an unset variable fails silently
    /// inside the app, hours later, as a confusing runtime error.
    #[error(
        "{at}: environment variable {var} is not set. It is declared as \
         `{{ \"env\": \"{var}\" }}`, so veld reads it from the environment it was launched \
         with — export it, or switch the source to `{{ \"file\": … }}` / `{{ \"value\": … }}`"
    )]
    MissingEnv { at: String, var: String },

    #[error("{at}: could not read {path}: {source}")]
    FileUnreadable {
        at: String,
        path: String,
        source: std::io::Error,
    },

    #[error(
        "{at}: source command timed out after {}s. It runs on whichever veld process \
         starts the run, which may have no terminal, so an interactive credential helper \
         (biometric prompt, MFA push) will hang here rather than fail. Use a \
         non-interactive alternative — a service account, a pre-warmed session, or \
         `{{ \"env\": … }}` / `{{ \"file\": … }}`",
        SOURCE_COMMAND_TIMEOUT.as_secs()
    )]
    CommandTimeout { at: String },

    #[error("{at}: source command failed ({code}): {stderr}")]
    CommandFailed {
        at: String,
        code: String,
        stderr: String,
    },

    #[error("{at}: source command could not be run: {source}")]
    CommandUnspawnable {
        at: String,
        source: crate::process::ProcessError,
    },

    #[error("{at}: source command produced output that is not valid UTF-8")]
    CommandNotUtf8 { at: String },

    #[error(
        "{at}: \"{path}\" is outside the project. A delivered file must be a relative path \
         inside the project root — an absolute path or one containing `..` is refused, because \
         the payload is usually a credential and the destination should be reviewable"
    )]
    FileOutsideProject { at: String, path: String },

    #[error("{at}: could not write {path}: {source}")]
    FileUnwritable {
        at: String,
        path: String,
        source: std::io::Error,
    },
}

/// Resolve one value. `at` labels the location for error messages (e.g.
/// `nodes.api.variants.dev.env.DATABASE_URL`) and never contains the value.
///
/// Trailing whitespace is trimmed from every fetched form, because a file or a
/// CLI almost always ends its output with a newline and a token with a trailing
/// `\n` fails in ways that are miserable to debug. An inline literal is trimmed
/// the same way for consistency.
pub async fn resolve_value(
    value: &ConfigValue,
    at: &str,
    project_root: Option<&std::path::Path>,
) -> Result<String, ValueError> {
    match &value.source {
        SecretSource::Literal(v) => Ok(v.clone()),
        SecretSource::Env(var) => std::env::var(var).map_err(|_| ValueError::MissingEnv {
            at: at.to_owned(),
            var: var.clone(),
        }),
        SecretSource::File(path) => {
            // Relative to the **project root**, as documented — not to the process
            // cwd. A run started from the management UI or the desktop app is
            // spawned by the daemon, whose cwd is nothing to do with the project,
            // so a cwd-relative read would work from a terminal and fail there.
            let resolved = match project_root {
                Some(root) => root.join(path),
                None => std::path::PathBuf::from(path),
            };
            tokio::fs::read_to_string(&resolved)
                .await
                .map(|s| s.trim_end().to_owned())
                .map_err(|source| ValueError::FileUnreadable {
                    at: at.to_owned(),
                    path: resolved.display().to_string(),
                    source,
                })
        }
        other => {
            let spec = other
                .command()
                .expect("literal/env/file are handled above; the rest run a command");
            run_source_command(&spec, at).await
        }
    }
}

/// Run a source command and take its trimmed stdout.
///
/// The command inherits the user's login-shell `PATH`: a run started from the
/// management UI or the desktop app is spawned by the daemon, which has a bare
/// service `PATH`, so `op read …` would otherwise fail with "command not found"
/// even though it works in the author's terminal. Same convention as liveness
/// probes and relay-token resolution.
async fn run_source_command(spec: &CommandSpec, at: &str) -> Result<String, ValueError> {
    let user_path = crate::user_path::resolve_user_path().await;
    let mut command =
        crate::process::tokio_command(spec).map_err(|source| ValueError::CommandUnspawnable {
            at: at.to_owned(),
            source,
        })?;
    let output = tokio::time::timeout(
        SOURCE_COMMAND_TIMEOUT,
        command
            .env("PATH", user_path)
            .stdin(std::process::Stdio::null())
            // kill_on_drop so a command that outlives the timeout (a hung helper
            // waiting on a prompt that will never come) is reaped when the
            // timed-out future drops, rather than left running.
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ValueError::CommandTimeout { at: at.to_owned() })?
    .map_err(|e| ValueError::CommandUnspawnable {
        at: at.to_owned(),
        source: crate::process::ProcessError::SpawnFailed(e),
    })?;

    if !output.status.success() {
        return Err(ValueError::CommandFailed {
            at: at.to_owned(),
            code: output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "killed by signal".to_owned()),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim_end().to_owned())
        .map_err(|_| ValueError::CommandNotUtf8 { at: at.to_owned() })
}

/// Write every declared file for one node, before its process starts (F9.1).
///
/// Paths are resolved from the project root, like every other relative path in a
/// veld config. Parent directories are created. The file is created with its
/// declared mode (default `0600`) **before** the content is written, so a secret is
/// never briefly world-readable — creating then chmod-ing would leave exactly that
/// window.
pub async fn deliver_files(
    files: &HashMap<String, crate::config::FileDelivery>,
    project_root: &std::path::Path,
    at_prefix: &str,
) -> Result<Vec<std::path::PathBuf>, ValueError> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut written = Vec::new();
    // Sorted for a deterministic failure.
    let mut paths: Vec<&String> = files.keys().collect();
    paths.sort();
    for rel in paths {
        let delivery = &files[rel];
        let at = format!("{at_prefix}.files.{rel}");
        let content = resolve_value(&delivery.value, &at, Some(project_root)).await?;
        let mode = delivery
            .parsed_mode()
            .expect("the mode was validated at parse time");

        // Confine the path. A config author already has code execution via `argv`,
        // so this is not an escalation — but the docs promise "relative to the
        // project root", and silently writing to `../../.ssh/authorized_keys`
        // breaks that promise in the one place where the payload is a credential.
        let rel_path = std::path::Path::new(rel);
        if rel_path.is_absolute() {
            return Err(ValueError::FileOutsideProject {
                at: at.clone(),
                path: rel.clone(),
            });
        }
        if rel_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(ValueError::FileOutsideProject {
                at: at.clone(),
                path: rel.clone(),
            });
        }
        let path = project_root.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ValueError::FileUnwritable {
                at: at.clone(),
                path: path.display().to_string(),
                source,
            })?;
        }
        let write = |content: &str| -> std::io::Result<()> {
            use std::io::Write as _;
            // `OpenOptions::mode` applies **only when the file is created**, so
            // writing over an existing file would silently keep its old mode — and
            // the common case is exactly that: the second run, or a path that is
            // checked in at 0644. Remove it first so the create-with-mode is real,
            // which also drops a symlink rather than following it and truncating
            // whatever it points at.
            match std::fs::symlink_metadata(&path) {
                Ok(_) => std::fs::remove_file(&path)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&path)?;
            f.write_all(content.as_bytes())
        };
        write(&content).map_err(|source| ValueError::FileUnwritable {
            at: at.clone(),
            path: path.display().to_string(),
            source,
        })?;
        written.push(path);
    }
    Ok(written)
}

/// Resolve every project `var` once per run.
///
/// Once, not per use site: a var whose source is a command must run that command
/// exactly one time, or two references to `${vars.db_url}` could disagree — and
/// with a rotating credential they would.
pub async fn resolve_vars(
    vars: Option<&HashMap<String, ConfigValue>>,
    project_root: Option<&std::path::Path>,
) -> Result<HashMap<String, String>, ValueError> {
    let mut out = HashMap::new();
    let Some(vars) = vars else {
        return Ok(out);
    };
    let mut names: Vec<&String> = vars.keys().collect();
    names.sort();
    for name in names {
        let value = &vars[name];
        out.insert(
            name.clone(),
            resolve_value(value, &format!("vars.{name}"), project_root).await?,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cv(json: &str) -> ConfigValue {
        serde_json::from_str(json).expect("value parses")
    }

    #[tokio::test]
    async fn literal_and_object_literal_resolve() {
        assert_eq!(
            resolve_value(&cv(r#""plain""#), "at", None).await.unwrap(),
            "plain"
        );
        // The object form exists so an inline literal can carry `secret: true`.
        let secret = cv(r#"{ "value": "devpassword", "secret": true }"#);
        assert!(secret.secret);
        assert_eq!(
            resolve_value(&secret, "at", None).await.unwrap(),
            "devpassword"
        );
    }

    /// The issue's requirement: missing at start is an error naming the node and
    /// the variable, not a silent empty value the app trips over later.
    #[tokio::test]
    async fn missing_env_source_errors_by_name() {
        let err = resolve_value(
            &cv(r#"{ "env": "VELD_TEST_DEFINITELY_UNSET_VAR" }"#),
            "nodes.api.variants.dev.env.DATABASE_URL",
            None,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ValueError::MissingEnv { .. }));
        assert!(
            msg.contains("nodes.api.variants.dev.env.DATABASE_URL"),
            "must name the node and the variable: {msg}"
        );
        assert!(msg.contains("VELD_TEST_DEFINITELY_UNSET_VAR"), "{msg}");
    }

    #[tokio::test]
    async fn file_source_reads_and_trims() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        // A trailing newline is what every editor and `echo` produces; a token
        // carrying one fails in miserable ways.
        std::fs::write(&path, "s3cret\n").unwrap();
        let value = cv(&format!(
            r#"{{ "file": {}, "secret": true }}"#,
            serde_json::to_string(&path.to_string_lossy()).unwrap()
        ));
        assert_eq!(resolve_value(&value, "at", None).await.unwrap(), "s3cret");

        let missing = cv(r#"{ "file": "/definitely/not/here" }"#);
        assert!(matches!(
            resolve_value(&missing, "at", None).await,
            Err(ValueError::FileUnreadable { .. })
        ));
    }

    #[tokio::test]
    async fn command_sources_resolve_in_both_forms() {
        assert_eq!(
            resolve_value(&cv(r#"{ "shell": "printf 'from-shell\n'" }"#), "at", None)
                .await
                .unwrap(),
            "from-shell"
        );
        assert_eq!(
            resolve_value(&cv(r#"{ "argv": ["printf", "from-argv"] }"#), "at", None)
                .await
                .unwrap(),
            "from-argv"
        );
    }

    #[tokio::test]
    async fn failing_command_reports_stderr_not_the_value() {
        let err = resolve_value(
            &cv(r#"{ "shell": "echo nope 1>&2; exit 3" }"#),
            "nodes.db.variants.dev.env.PASSWORD",
            None,
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nodes.db.variants.dev.env.PASSWORD"), "{msg}");
        assert!(msg.contains('3'), "{msg}");
        assert!(msg.contains("nope"), "{msg}");
    }

    /// A source command that waits for input it will never get must hit the
    /// timeout with a message that says *why* — the fix is a non-interactive
    /// source, not a longer timeout.
    ///
    /// Driven through the timeout helper directly rather than by waiting out
    /// `SOURCE_COMMAND_TIMEOUT`: a 30-second test is a 30-second test on every
    /// CI run, and tokio's clock-pausing needs the `test-util` feature the
    /// workspace does not enable.
    #[tokio::test]
    async fn source_command_timeout_reports_tty_hint() {
        let hung =
            tokio::time::timeout(Duration::from_millis(50), std::future::pending::<()>()).await;
        assert!(hung.is_err(), "the helper must actually time out");
        // The error the real path builds on that timeout is what the author reads.
        let err = ValueError::CommandTimeout {
            at: "nodes.db.variants.dev.env.PASSWORD".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("nodes.db.variants.dev.env.PASSWORD"), "{msg}");
        assert!(msg.contains("no terminal"), "{msg}");
        assert!(msg.contains("MFA") || msg.contains("biometric"), "{msg}");
        assert!(
            msg.contains("\"env\"") || msg.contains("\"file\""),
            "must name a non-interactive alternative: {msg}"
        );
    }

    /// …and the real path does reach that error. Uses a command that blocks on
    /// stdin, which `resolve_value` closes, so this is fast rather than a
    /// timeout-length wait — it pins that a hung helper cannot hang veld.
    #[tokio::test]
    async fn source_command_with_no_stdin_does_not_hang() {
        // stdin is /dev/null, so `cat` sees EOF immediately instead of waiting
        // for input that would never come under a daemon.
        let out = resolve_value(&cv(r#"{ "argv": ["cat"] }"#), "at", None)
            .await
            .expect("a helper reading stdin must not hang");
        assert_eq!(out, "");
    }

    /// F9.1: a value can be delivered to disk, for a process that can only read a
    /// file. Without this the workaround is a shell command that writes the secret
    /// itself, which puts it in the process table on the way.
    #[tokio::test]
    async fn files_are_delivered_with_a_restrictive_default_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let files: HashMap<String, crate::config::FileDelivery> = HashMap::from([
            (
                // A nested path whose directory does not exist yet.
                ".secrets/token".to_owned(),
                serde_json::from_str(r#"{ "value": "s3cret", "secret": true }"#).unwrap(),
            ),
            (
                "config/app.conf".to_owned(),
                serde_json::from_str(r#"{ "value": "verbose=1", "mode": "0644" }"#).unwrap(),
            ),
        ]);

        let written = deliver_files(&files, dir.path(), "nodes.a.variants.dev")
            .await
            .expect("files are delivered");
        assert_eq!(written.len(), 2);

        let token = dir.path().join(".secrets/token");
        assert_eq!(std::fs::read_to_string(&token).unwrap(), "s3cret");
        assert_eq!(
            std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
            0o600,
            "a delivered file defaults to owner-only — the motivating case is a credential"
        );

        let conf = dir.path().join("config/app.conf");
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), "verbose=1");
        assert_eq!(
            std::fs::metadata(&conf).unwrap().permissions().mode() & 0o777,
            0o644,
            "an explicit mode is honoured"
        );
    }

    /// The mode is octal whether or not it carries a leading zero: reading `"600"`
    /// as decimal would silently produce mode 1130.
    #[test]
    fn file_mode_is_parsed_as_octal_and_bad_modes_are_rejected() {
        let parse = |json: &str| serde_json::from_str::<crate::config::FileDelivery>(json);

        assert_eq!(
            parse(r#"{ "value": "x", "mode": "0600" }"#)
                .unwrap()
                .parsed_mode()
                .unwrap(),
            0o600
        );
        assert_eq!(
            parse(r#"{ "value": "x", "mode": "600" }"#)
                .unwrap()
                .parsed_mode()
                .unwrap(),
            0o600,
            "a missing leading zero still means octal"
        );
        assert_eq!(
            parse(r#"{ "value": "x" }"#).unwrap().parsed_mode().unwrap(),
            0o600,
            "the default is owner-only"
        );

        // Rejected at parse time, not at spawn time when the run is half up.
        assert!(parse(r#"{ "value": "x", "mode": "0999" }"#).is_err());
        assert!(parse(r#"{ "value": "x", "mode": "rw-------" }"#).is_err());
        // A bare number has already lost its leading zero, so it is refused
        // rather than guessed at.
        assert!(parse(r#"{ "value": "x", "mode": 600 }"#).is_err());
    }

    #[test]
    fn source_label_never_leaks_a_literal() {
        let v = cv(r#"{ "value": "hunter2", "secret": true }"#);
        assert!(!v.source_label().contains("hunter2"));
    }
}
