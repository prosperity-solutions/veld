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
}

/// Resolve one value. `at` labels the location for error messages (e.g.
/// `nodes.api.variants.dev.env.DATABASE_URL`) and never contains the value.
///
/// Trailing whitespace is trimmed from every fetched form, because a file or a
/// CLI almost always ends its output with a newline and a token with a trailing
/// `\n` fails in ways that are miserable to debug. An inline literal is trimmed
/// the same way for consistency.
pub async fn resolve_value(value: &ConfigValue, at: &str) -> Result<String, ValueError> {
    match &value.source {
        SecretSource::Literal(v) => Ok(v.clone()),
        SecretSource::Env(var) => std::env::var(var).map_err(|_| ValueError::MissingEnv {
            at: at.to_owned(),
            var: var.clone(),
        }),
        SecretSource::File(path) => tokio::fs::read_to_string(path)
            .await
            .map(|s| s.trim_end().to_owned())
            .map_err(|source| ValueError::FileUnreadable {
                at: at.to_owned(),
                path: path.clone(),
                source,
            }),
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

/// One node's resolved environment, plus which of its keys hold secrets.
#[derive(Debug, Clone, Default)]
pub struct ResolvedEnv {
    pub values: HashMap<String, String>,
    /// Keys whose values were declared `secret: true`. Carried separately so the
    /// values themselves stay plain `String` — the share manifest and run-state
    /// wire types must remain serde-lenient, so sensitivity is a flag beside the
    /// value, never a wrapper type around it.
    pub secret_keys: Vec<String>,
}

/// Resolve every value in one node's environment.
///
/// Resolution is **eager and fail-fast**: a missing `env` source or a hanging
/// helper is reported before anything spawns, rather than surfacing as an
/// inexplicable application error minutes into a run.
pub async fn resolve_env_values(
    env: &HashMap<String, ConfigValue>,
    at_prefix: &str,
) -> Result<ResolvedEnv, ValueError> {
    let mut out = ResolvedEnv::default();
    // Sorted so an error is deterministic: with two broken sources, the same one
    // is reported every time.
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    for key in keys {
        let value = &env[key];
        let at = format!("{at_prefix}.env.{key}");
        out.values
            .insert(key.clone(), resolve_value(value, &at).await?);
        if value.secret {
            out.secret_keys.push(key.clone());
        }
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
            resolve_value(&cv(r#""plain""#), "at").await.unwrap(),
            "plain"
        );
        // The object form exists so an inline literal can carry `secret: true`.
        let secret = cv(r#"{ "value": "devpassword", "secret": true }"#);
        assert!(secret.secret);
        assert_eq!(resolve_value(&secret, "at").await.unwrap(), "devpassword");
    }

    /// The issue's requirement: missing at start is an error naming the node and
    /// the variable, not a silent empty value the app trips over later.
    #[tokio::test]
    async fn missing_env_source_errors_by_name() {
        let err = resolve_value(
            &cv(r#"{ "env": "VELD_TEST_DEFINITELY_UNSET_VAR" }"#),
            "nodes.api.variants.dev.env.DATABASE_URL",
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
        assert_eq!(resolve_value(&value, "at").await.unwrap(), "s3cret");

        let missing = cv(r#"{ "file": "/definitely/not/here" }"#);
        assert!(matches!(
            resolve_value(&missing, "at").await,
            Err(ValueError::FileUnreadable { .. })
        ));
    }

    #[tokio::test]
    async fn command_sources_resolve_in_both_forms() {
        assert_eq!(
            resolve_value(&cv(r#"{ "shell": "printf 'from-shell\n'" }"#), "at")
                .await
                .unwrap(),
            "from-shell"
        );
        assert_eq!(
            resolve_value(&cv(r#"{ "argv": ["printf", "from-argv"] }"#), "at")
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
        let out = resolve_value(&cv(r#"{ "argv": ["cat"] }"#), "at")
            .await
            .expect("a helper reading stdin must not hang");
        assert_eq!(out, "");
    }

    #[tokio::test]
    async fn resolve_env_values_collects_secret_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k");
        std::fs::write(&path, "filevalue").unwrap();
        let env: HashMap<String, ConfigValue> = HashMap::from([
            ("PLAIN".to_owned(), cv(r#""visible""#)),
            (
                "SECRET".to_owned(),
                cv(r#"{ "value": "hidden", "secret": true }"#),
            ),
            (
                "FROM_FILE".to_owned(),
                cv(&format!(
                    r#"{{ "file": {}, "secret": true }}"#,
                    serde_json::to_string(&path.to_string_lossy()).unwrap()
                )),
            ),
        ]);
        let resolved = resolve_env_values(&env, "nodes.a.variants.dev")
            .await
            .unwrap();
        assert_eq!(resolved.values.get("PLAIN").unwrap(), "visible");
        assert_eq!(resolved.values.get("SECRET").unwrap(), "hidden");
        assert_eq!(resolved.values.get("FROM_FILE").unwrap(), "filevalue");
        let mut secrets = resolved.secret_keys.clone();
        secrets.sort();
        assert_eq!(secrets, vec!["FROM_FILE", "SECRET"]);
    }

    #[test]
    fn value_forms_round_trip_and_reject_ambiguity() {
        // The terse form stays terse on the way out.
        assert_eq!(
            serde_json::to_string(&ConfigValue::literal("x")).unwrap(),
            r#""x""#
        );
        // A secret literal has to use the object form to carry the flag.
        assert_eq!(
            serde_json::to_string(&cv(r#"{ "value": "x", "secret": true }"#)).unwrap(),
            r#"{"value":"x","secret":true}"#
        );
        // Two sources at once is ambiguous.
        assert!(serde_json::from_str::<ConfigValue>(r#"{ "env": "A", "file": "/b" }"#).is_err());
        // No source at all says nothing.
        assert!(serde_json::from_str::<ConfigValue>(r#"{ "secret": true }"#).is_err());
        // An unknown source key is a typo, not a new provider.
        assert!(serde_json::from_str::<ConfigValue>(r#"{ "vault": "x" }"#).is_err());
    }

    #[test]
    fn source_label_never_leaks_a_literal() {
        let v = cv(r#"{ "value": "hunter2", "secret": true }"#);
        assert!(!v.source_label().contains("hunter2"));
    }
}
