//! `veld-sign` — sign a veld helper binary with the org's ed25519 key.
//!
//! Reads a binary and the org private key (PKCS#8 PEM, from an env var — the
//! GitHub `SIGNING_PRIVATE_KEY` secret — or a file), and writes a detached
//! `<binary>.sig` containing the raw 64-byte ed25519 signature.
//!
//! The running root helper (`veld-helper`) verifies that signature against its
//! embedded public key before ever relaunching onto a changed on-disk binary
//! (see `crates/veld-helper/src/signing.rs`). CI runs this over the *final*
//! shipped bytes — on macOS that is the ad-hoc re-signed binary, so install.sh's
//! later re-sign is a byte-idempotent no-op and the `.sig` still matches.
//!
//! Diagnosis is a feature here, not a nicety. This tool runs in exactly one
//! place — `release.yml`'s `Package client binaries` — and only on a tagged
//! release, so a bad key is discovered at the most expensive possible moment.
//! v16.58.1 failed with a raw RustCrypto ASN.1 string that named the label it
//! wanted and never the one it got, reading like a corrupt key when the actual
//! mistake was the wrong *file* in the secret. Every failure below therefore
//! names the label found, the label expected, and the source read — and never
//! any part of the key itself.
//!
//! **That last rule is load-bearing, and every way it broke while this was
//! written looked like careful code.** All four were found by review, not by
//! the compiler, and each has a named guard now:
//!
//!   * interpolating an upstream `Display` (`{e}`) — `der` quotes the input it
//!     rejected, both as a byte and as a length → [`key_parse_problem`];
//!   * reading the variable with `std::env::var`, whose `NotUnicode` error
//!     Debug-formats the entire value → [`read_key`];
//!   * echoing anything from `argv`, because a key pasted where a *path* or a
//!     *variable name* belongs — or passed positionally, since a PEM starts
//!     with `-` — is one slip away in a workflow edit → [`echoable`];
//!   * quoting the PEM label without bounding it → [`pem_header`].
//!
//! The shape of the rule: **a message is assembled only from fixed strings, a
//! filtered label, and a redacted source.** Anything else that reaches stderr
//! should be assumed to quote its input until proven otherwise.
//!
//! `tests/smoke.rs` drives the whole path through the process boundary on every
//! PR, so a break lands there instead of in a release.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ed25519_dalek::Signer;
use ed25519_dalek::pkcs8::DecodePrivateKey;

const USAGE: &str = "usage: veld-sign --key-env <VAR> | --key-file <path> <binary>";

/// Exit codes. The split is for whoever is reading a failed workflow log —
/// `release.yml` runs this under `bash -e` and only sees zero vs non-zero.
///
/// `EXIT_BAD_INPUT` is "I ran, and could not produce a signature from what you
/// gave me": a key that is missing, wrongly encoded, or not ed25519, or a
/// binary that cannot be read or whose `.sig` cannot be written.
/// `EXIT_USAGE` is "the command line itself is wrong", before any input is
/// touched. Nothing downstream branches on these, but they are asserted in
/// `tests/smoke.rs` so the split cannot drift unnoticed.
const EXIT_BAD_INPUT: u8 = 1;
const EXIT_USAGE: u8 = 2;

/// The one PEM label this tool accepts. Deliberately not widened (#339): one
/// format with a clear error is easier to reason about for a signing tool than
/// several with a vague one.
const EXPECTED_LABEL: &str = "PRIVATE KEY";

/// Longest PEM label ever quoted back in an error. Real labels are far shorter
/// (`ENCRYPTED PRIVATE KEY` is 21 characters), so anything longer is not a
/// label and is not something we are willing to echo.
const MAX_LABEL_LEN: usize = 40;

/// Stand-in for an argument that is not clearly a name, a path, or a flag.
const REDACTED: &str = "<redacted: that is not a name or a path this tool would accept>";

/// Longest name — a variable, a path segment — this tool will echo.
/// `SIGNING_PRIVATE_KEY` is 19 and `veld-helper` is 11; the shortest encoding of
/// the key it must never echo is 64 characters.
const MAX_NAME_LEN: usize = 40;

/// Most alphanumeric characters a single path segment may carry and still be
/// printed. Real filenames are modest — `veld-helper` has 10,
/// `validate-workflow-gates.py` 22 — while an encoded key packs the maximum it
/// can into every character.
///
/// This exists for **URL-safe** base64, whose alphabet — unlike standard
/// base64, base32 and hex — does contain `-` and `_`, so it is the one encoding
/// that can satisfy the separator rule below. Unchunked it is far too long, but
/// split across `/` its pieces would otherwise read as filenames.
const MAX_SEGMENT_ALNUM: usize = 24;

/// A path segment safe to print.
///
/// **The load-bearing clause is that the segment must contain a `.`, `-` or
/// `_`.** Standard base64, base32 and hex use none of those characters, so no
/// key in any of those encodings can satisfy it. The rule holds by construction
/// rather than by calibration, which is what the four earlier versions lacked:
/// each judged some *property* of key material — does it contain a slash, is it
/// long, does it mix case, is it one long run — and each was defeated by an
/// encoding that happened not to have it. Judging what a *filename* has instead
/// cannot be dodged by re-encoding.
///
/// There is deliberately no allowance for short separator-free names like
/// `dist` or `tmp`: permitting them let a key chunked into eight-character
/// pieces pass as a directory tree. That was found by the randomised sweep
/// rather than by reasoning about it, which is the argument for having one.
///
/// **The residual, stated rather than implied.** URL-safe base64 *is* an
/// alphabet containing `-` and `_`. A key in it is far too long to print whole,
/// but split on `/` into filename-sized pieces, one piece could surface. That
/// takes a `/` placed by hand: no encoding produces both a `/` and a `.`/`-`/`_`
/// — standard base64 has the slash and none of the others, URL-safe has the
/// others and never a slash — which is what
/// `no_encoding_supplies_both_a_slash_and_a_name_character` checks, and what
/// makes this unreachable by the pasted-secret accidents this tool exists to
/// diagnose. `url_safe_base64_is_bounded_even_when_chunked_to_look_like_a_path`
/// pins how much a hand-built input could ever surface.
fn is_named_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= MAX_NAME_LEN
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && segment.chars().any(|c| matches!(c, '.' | '-' | '_'))
        && segment
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .count()
            <= MAX_SEGMENT_ALNUM
}

// Text from `argv` is rendered for an error message only when it is clearly the
// *kind of thing* the flag it followed is supposed to be — a name, a path, or a
// flag. Everything else becomes [`REDACTED`].
//
// **`argv` is as untrusted as the key here.** This tool takes the key by *name*
// (`--key-env`) and by *path* (`--key-file`), and three slips put the key itself
// on the command line:
//
//   * `--key-file "$SIGNING_PRIVATE_KEY"` — the key where a path belongs;
//   * `--key-env "$SIGNING_PRIVATE_KEY"` — the key where a variable *name*
//     belongs, which then reads as an unset variable;
//   * the key as a bare argument, which parses as an unknown flag because a PEM
//     begins with `-`, or as the `<binary>` to sign if it does not.
//
// Each is one character away from the correct invocation, and each put the whole
// key into a message bound for a public workflow log.

/// An environment variable name, echoed only if it is actually an identifier.
fn echoable_name(value: &str) -> &str {
    let ok = !value.is_empty()
        && value.len() <= MAX_NAME_LEN
        && !value.starts_with(|c: char| c.is_ascii_digit())
        && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok { value } else { REDACTED }
}

/// A filesystem path, echoed whole when every segment is one this tool would
/// print, and otherwise narrowed to the file's own name.
///
/// **Narrowing is the normal outcome, not the exception.** Ordinary directory
/// names — `dist`, `target`, `release`, `runner`, and macOS `$TMPDIR`'s
/// 30-character random component — carry no `.`, `-` or `_`, so `dist/veld-helper`
/// renders as `…/veld-helper`. That is the deliberate trade: no rule can tell a
/// directory name from encoded bytes without being beaten by some encoding, and
/// a key chunked into short pieces looks exactly like a directory tree. The flag
/// and the filename — what an operator acts on — survive; the directory does not.
fn echoable_path(value: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let segments: Vec<&str> = value.split('/').filter(|s| !s.is_empty()).collect();
    // The whole value is bounded as well as each segment: a key chunked into
    // filename-sized pieces can pass every segment individually and still
    // reassemble the key across them, which is precisely what the randomised
    // sweep produced. Bounding the total means at most one piece is ever shown.
    let alnum = value.chars().filter(|c| c.is_ascii_alphanumeric()).count();
    if !segments.is_empty()
        && alnum <= MAX_SEGMENT_ALNUM
        && segments.iter().all(|s| is_named_segment(s))
    {
        return Cow::Borrowed(value);
    }
    match segments.last() {
        Some(name) if is_named_segment(name) => Cow::Owned(format!("…/{name}")),
        _ => Cow::Borrowed(REDACTED),
    }
}

/// An unrecognised flag, echoed so a typo is visible — but a PEM starts with
/// `-`, so this is one of the three doors above.
fn echoable_flag(value: &str) -> &str {
    let ok = value.len() <= MAX_NAME_LEN
        && value
            .trim_start_matches('-')
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { value } else { REDACTED }
}

/// Where the key text came from, named with the flag the operator actually
/// typed — so a failure says immediately whether to go look at a CI secret or
/// at a file on disk.
///
/// Holds the *source* of the key, never the key — and renders even the source
/// through [`echoable`], because the source itself comes from `argv`.
enum KeySource {
    Env(String),
    File(PathBuf),
}

impl fmt::Display for KeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeySource::Env(var) => write!(f, "--key-env {}", echoable_name(var)),
            KeySource::File(path) => {
                write!(f, "--key-file {}", echoable_path(&path.to_string_lossy()))
            }
        }
    }
}

/// A failure worth explaining: what went wrong, and — when the PEM label
/// identifies which mistake this is — what to do about it.
///
/// Both fields are built only from the PEM label (filtered by [`pem_header`]),
/// the [`KeySource`], and fixed strings. No key material ever reaches either
/// one; `key_body_never_appears_in_an_error` is what holds that line.
#[derive(Debug)]
struct SignError {
    problem: String,
    hint: Option<&'static str>,
}

impl SignError {
    fn new(problem: impl Into<String>) -> Self {
        Self {
            problem: problem.into(),
            hint: None,
        }
    }

    fn with_hint(problem: impl Into<String>, hint: &'static str) -> Self {
        Self {
            problem: problem.into(),
            hint: Some(hint),
        }
    }

    /// The operator-facing rendering, exactly as `main` prints it to stderr.
    fn render(&self) -> String {
        match self.hint {
            Some(hint) => format!("error: {}\nhint: {hint}", self.problem),
            None => format!("error: {}", self.problem),
        }
    }
}

/// What the first `-----BEGIN …-----` line in the input turned out to be.
enum PemHeader<'a> {
    /// A label safe to quote back in an error, e.g. `PRIVATE KEY`.
    Label(&'a str),
    /// A `-----BEGIN …-----` line whose label this tool will not repeat.
    ///
    /// RFC 7468 allows far more than the labels below — any printable ASCII bar
    /// `-`, in any case — so this is *not* the same question as "is it valid
    /// PEM". It is "would echoing this put attacker- or accident-chosen text
    /// into a public CI log", and the answer is no only for the shape every
    /// real key label has. The header is still reported as *present*: saying
    /// there was none would be its own misleading message.
    Unquotable,
    /// A `-----BEGIN`/`-----END` boundary line that does not start at column 0.
    ///
    /// RFC 7468 boundaries are anchored, so `from_pkcs8_pem` rejects the whole
    /// document — but the label and the body are both fine, and reporting this
    /// as a corrupt body is exactly the misdiagnosis #339 exists to remove. A
    /// key pasted into a YAML block scalar or an indented heredoc lands here.
    ///
    /// Only the *expected* label reaches this variant: an indented wrong label
    /// comes back as `Label`, because which file you grabbed is the more useful
    /// thing to be told. See `a_wrong_label_wins_over_indentation`.
    Indented,
    /// The value opens with a UTF-8 byte-order mark, so the first line is not a
    /// boundary however much it looks like one in an editor. `U+FEFF` is not
    /// `char::is_whitespace`, so nothing upstream trims it away.
    ByteOrderMark,
    /// No `-----BEGIN …-----` line anywhere in the input.
    Missing,
}

/// Classify the first PEM block in `input` — `PRIVATE KEY` for a
/// `-----BEGIN PRIVATE KEY-----` header.
///
/// The label is bounded and character-filtered *here* rather than at the print
/// site, because this value is the one piece of the input that reaches an error
/// message and the input is a private key. A permissive reader handed something
/// that is not PEM would happily quote an arbitrary slice of it into a CI log.
fn pem_header(input: &str) -> PemHeader<'_> {
    if input.starts_with('\u{feff}') {
        return PemHeader::ByteOrderMark;
    }
    let Some(line) = input
        .lines()
        .find(|line| line.trim_start().starts_with("-----BEGIN "))
    else {
        return PemHeader::Missing;
    };
    let Some(label) = line
        .trim()
        .strip_prefix("-----BEGIN ")
        .and_then(|rest| rest.strip_suffix("-----"))
    else {
        // `-----BEGIN ` with no closing `-----`: a header in spirit only.
        return PemHeader::Unquotable;
    };
    let quotable = !label.is_empty()
        && label.len() <= MAX_LABEL_LEN
        && label
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' ');
    if !quotable {
        return PemHeader::Unquotable;
    }
    // Indentation only matters once the label is the one we want: a wrong label
    // is the more useful thing to report, indented or not.
    //
    // Checked across *every* boundary line, not just the first. A re-indent that
    // caught the body and the END line but not BEGIN is still rejected by the
    // parser, and reporting that as a corrupt body is the same misdiagnosis this
    // variant exists to remove. Trimming the label first so that a boundary
    // which is both indented and padded reports the indentation, whose fix
    // ("start at column 0, no extra spaces") covers both — telling the operator
    // only about the spaces would leave them failing again after doing exactly
    // what they were told.
    if label.trim() == EXPECTED_LABEL && input.lines().any(is_indented_boundary) {
        return PemHeader::Indented;
    }
    PemHeader::Label(label)
}

/// A `-----BEGIN`/`-----END` line that does not start at column 0.
fn is_indented_boundary(line: &str) -> bool {
    let trimmed = line.trim_start();
    (trimmed.starts_with("-----BEGIN") || trimmed.starts_with("-----END"))
        && line.starts_with(char::is_whitespace)
}

/// What to do about a label we recognise as one specific operator mistake.
/// An unrecognised label still gets the expected/got/source message; it just
/// has no advice to add.
fn label_hint(label: &str) -> Option<&'static str> {
    match label {
        "PUBLIC KEY" => Some("this looks like the .pub, not the private key"),
        "OPENSSH PRIVATE KEY" => {
            Some("this is an OpenSSH key; convert it with `ssh-keygen -p -m PKCS8 -f <key>`")
        }
        "ENCRYPTED PRIVATE KEY" => Some("this key is passphrase-protected; decrypt it first"),
        _ => None,
    }
}

/// Why a correctly-labelled PKCS#8 body would not parse, as a fixed string.
///
/// The upstream `Display` is deliberately **not** interpolated here. It reaches
/// stderr, stderr reaches a public workflow log, and `der`'s own renderings
/// quote the input:
///
///   * `ErrorKind::TagUnknown` prints the offending byte —
///     `unknown/unsupported ASN.1 DER tag: 0x11` — which for a bare 32-byte
///     seed pasted under the right header is literally `seed[0]`;
///   * `ErrorKind::Incomplete` prints the DER length and the decoded body
///     length — `expected 48, actual 30`.
///
/// #339 forbids both: "not the body, not a prefix of it, not a length that
/// narrows it". Classifying by variant keeps the diagnosis while saying only
/// what *shape* the input had, never what was in it.
fn key_parse_problem(e: &ed25519_dalek::pkcs8::Error) -> &'static str {
    use ed25519_dalek::pkcs8::Error;
    match e {
        Error::Asn1(_) => "the body is not valid DER, so the key is truncated or corrupt",
        Error::KeyMalformed => "the DER parsed, but the key inside it is not a usable ed25519 key",
        Error::ParametersMalformed => "the DER parsed, but its algorithm parameters are malformed",
        Error::PublicKey(_) => {
            "the DER parsed, but names a different algorithm — a PKCS#8 key, but not an ed25519 one"
        }
        // `pkcs8::Error` is `#[non_exhaustive]`; a variant added upstream must
        // still not reach stderr.
        _ => "the body did not parse as a PKCS#8 ed25519 private key",
    }
}

/// Turn PEM text into a signing key, diagnosing the wrong-file cases by label
/// before the parser sees anything — the parser's own error names the label it
/// wanted and never the one it got, which is what made v16.58.1 read as a
/// corrupt key rather than the wrong file.
fn parse_signing_key(
    key_pem: &str,
    source: &KeySource,
) -> Result<ed25519_dalek::SigningKey, SignError> {
    if key_pem.trim().is_empty() {
        return Err(SignError::with_hint(
            format!(
                "no key read from {source}: expected a PKCS#8 private key \
                 (\"BEGIN {EXPECTED_LABEL}\"), got an empty value"
            ),
            match source {
                KeySource::Env(_) => "the variable is unset, or the secret was uploaded empty",
                KeySource::File(_) => "the file is empty",
            },
        ));
    }

    let wrong_format = |got: &str| {
        format!(
            "wrong key format from {source}: expected a PKCS#8 private key \
             (\"BEGIN {EXPECTED_LABEL}\"), {got}"
        )
    };
    match pem_header(key_pem) {
        PemHeader::Label(EXPECTED_LABEL) => {}
        // A label that differs from the expected one only in whitespace renders
        // as two visually identical quoted strings in a log — the operator reads
        // `got "BEGIN PRIVATE KEY "` against `expected … "BEGIN PRIVATE KEY"`
        // and concludes the tool is broken. Name the actual difference instead.
        PemHeader::Label(found) if found.trim() == EXPECTED_LABEL => {
            return Err(SignError::with_hint(
                wrong_format("got the right label with stray whitespace around it"),
                "remove the extra spaces from the `-----BEGIN PRIVATE KEY-----` line",
            ));
        }
        PemHeader::Label(found) => {
            let problem = wrong_format(&format!("got \"BEGIN {found}\""));
            return Err(match label_hint(found) {
                Some(hint) => SignError::with_hint(problem, hint),
                None => SignError::new(problem),
            });
        }
        PemHeader::Unquotable => {
            return Err(SignError::with_hint(
                wrong_format("got a PEM block with an unrecognised label"),
                "the label is not repeated here because the input is a private key; \
                 the value must start with a `-----BEGIN PRIVATE KEY-----` line",
            ));
        }
        PemHeader::Indented => {
            return Err(SignError::with_hint(
                wrong_format("got the right label on an indented line"),
                "PEM boundaries must start at column 0 with no extra spaces — strip \
                 the leading whitespace a YAML block scalar or an indented heredoc adds",
            ));
        }
        PemHeader::ByteOrderMark => {
            return Err(SignError::with_hint(
                wrong_format("got a byte-order mark before the first line"),
                "re-save the key as UTF-8 without a BOM, or as plain ASCII",
            ));
        }
        PemHeader::Missing => {
            return Err(SignError::with_hint(
                wrong_format("found no PEM header at all"),
                "the value must start with a `-----BEGIN PRIVATE KEY-----` line",
            ));
        }
    }

    // Header is right, so this is a real attempt at the right kind of file.
    // The upstream error is classified rather than interpolated — see
    // `key_parse_problem`.
    ed25519_dalek::SigningKey::from_pkcs8_pem(key_pem).map_err(|e| {
        SignError::with_hint(
            format!(
                "invalid ed25519 private key from {source}: {}",
                key_parse_problem(&e)
            ),
            "the PEM header is right, so the problem is inside the key body itself",
        )
    })
}

/// Read the key text named by `source`, without ever rendering the value.
///
/// **`std::env::var` must not be used here.** Its `VarError::NotUnicode`
/// `Display` Debug-formats the whole `OsString`, so one stray non-UTF-8 byte in
/// the secret — a Latin-1 or UTF-16 paste, a BOM, raw DER uploaded instead of
/// PEM, a mangled copy made while *fixing* the v16.58.1 mistake — printed the
/// entire private key into a public workflow log. GitHub's secret masking is no
/// backstop: Debug escaping is exactly the value transformation that defeats it,
/// and once a byte inside the base64 line is mangled the printed text no longer
/// matches the registered secret at all. `var_os` + `into_string` keeps the bad
/// value in an `OsString` that is dropped without ever being formatted.
///
/// `read_to_string`'s own non-UTF-8 error carries no content ("stream did not
/// contain valid UTF-8"), but it routes to the same message so both sources
/// read alike.
fn read_key(source: &KeySource) -> Result<String, SignError> {
    let not_utf8 = || {
        SignError::with_hint(
            format!(
                "wrong key format from {source}: expected a PKCS#8 private key \
                 (\"BEGIN {EXPECTED_LABEL}\"), got bytes that are not valid UTF-8"
            ),
            "a PKCS#8 PEM is ASCII text; this may be raw DER, or a file saved in another encoding",
        )
    };
    match source {
        // An absent variable is the same operator mistake as an empty one — the
        // secret never reached the runner — so it takes the same diagnosis in
        // `parse_signing_key` rather than a separate message that reads like a
        // bug in the workflow.
        KeySource::Env(var) => match std::env::var_os(var) {
            None => Ok(String::new()),
            Some(raw) => raw.into_string().map_err(|_| not_utf8()),
        },
        KeySource::File(path) => std::fs::read_to_string(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::InvalidData {
                not_utf8()
            } else {
                SignError::new(format!("cannot read {source}: {e}"))
            }
        }),
    }
}

/// Sign `binary` with `key_pem` (PKCS#8 PEM) and write `<binary>.sig`.
fn sign_file(binary: &Path, key_pem: &str, source: &KeySource) -> Result<PathBuf, SignError> {
    let signing_key = parse_signing_key(key_pem, source)?;
    let data = std::fs::read(binary).map_err(|e| {
        SignError::new(format!(
            "cannot read {}: {e}",
            echoable_path(&binary.to_string_lossy())
        ))
    })?;
    let sig = signing_key.sign(&data);
    let sig_path = sig_path_for(binary);
    std::fs::write(&sig_path, sig.to_bytes()).map_err(|e| {
        SignError::new(format!(
            "cannot write {}: {e}",
            echoable_path(&sig_path.to_string_lossy())
        ))
    })?;
    Ok(sig_path)
}

/// `<path>.sig` — append, not replace any existing extension.
fn sig_path_for(binary: &Path) -> PathBuf {
    let mut s = binary.as_os_str().to_os_string();
    s.push(".sig");
    PathBuf::from(s)
}

fn main() -> ExitCode {
    let mut key_env: Option<String> = None;
    let mut key_file: Option<PathBuf> = None;
    let mut binary: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--key-env" => match args.next() {
                Some(v) => key_env = Some(v),
                None => return usage("--key-env requires a value"),
            },
            "--key-file" => match args.next() {
                Some(v) => key_file = Some(PathBuf::from(v)),
                None => return usage("--key-file requires a value"),
            },
            // A PEM begins with `-`, so a key handed over positionally lands
            // here rather than as the <binary> argument.
            other if other.starts_with('-') => {
                return usage(&format!("unknown flag: {}", echoable_flag(other)));
            }
            other => binary = Some(PathBuf::from(other)),
        }
    }

    let binary = match binary {
        Some(b) => b,
        None => return usage("missing <binary> path"),
    };

    let source = match (key_env, key_file) {
        (Some(_), Some(_)) => return usage("--key-env and --key-file are mutually exclusive"),
        (Some(var), None) => KeySource::Env(var),
        (None, Some(path)) => KeySource::File(path),
        (None, None) => return usage("provide --key-env or --key-file"),
    };

    let key_pem = match read_key(&source) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e.render());
            return ExitCode::from(EXIT_BAD_INPUT);
        }
    };

    match sign_file(&binary, &key_pem, &source) {
        Ok(sig_path) => {
            eprintln!(
                "signed {} -> {}",
                echoable_path(&binary.to_string_lossy()),
                echoable_path(&sig_path.to_string_lossy())
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", e.render());
            ExitCode::from(EXIT_BAD_INPUT)
        }
    }
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("error: {msg}\n{USAGE}");
    ExitCode::from(EXIT_USAGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL_SAFE_NO_PAD;
    use ed25519_dalek::pkcs8::EncodePrivateKey;
    use ed25519_dalek::{Verifier, VerifyingKey};
    use std::io::Read;

    // A throwaway ed25519 key (NOT the org key), generated with `openssl
    // genpkey -algorithm ED25519` — the exact format `veld update`/CI feed in
    // via the `SIGNING_PRIVATE_KEY` secret. Pins the production parse path.
    const TEST_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
        MC4CAQAwBQYDK2VwBCIEIG6yQcLcN3khrsV3dAHJmX/loSUSEoU9FVYNd4mqV+S1\n\
        -----END PRIVATE KEY-----\n";
    const TEST_PUBLIC: [u8; 32] = [
        0x57, 0x13, 0x06, 0xbc, 0x2b, 0xda, 0x86, 0xf9, 0x38, 0x55, 0xa7, 0xea, 0xda, 0xa7, 0x21,
        0x74, 0x11, 0x67, 0x09, 0x6c, 0xea, 0xb7, 0x03, 0x11, 0xd2, 0xf7, 0xd4, 0x33, 0x03, 0x0a,
        0xf0, 0xc7,
    ];

    fn env_source() -> KeySource {
        KeySource::Env("SIGNING_PRIVATE_KEY".into())
    }

    /// The base64 body of `TEST_PRIVATE_PEM`, with no header lines — i.e. the
    /// bytes that must never be echoed.
    fn key_body() -> String {
        TEST_PRIVATE_PEM
            .lines()
            .filter(|l| !l.trim_start().starts_with("-----"))
            .map(str::trim)
            .collect()
    }

    #[test]
    fn sign_file_writes_a_verifiable_detached_signature() {
        let dir = std::env::temp_dir().join(format!("veld-sign-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"macho/elf payload bytes").unwrap();

        let sig_path = sign_file(&binary, TEST_PRIVATE_PEM, &env_source()).unwrap();
        assert_eq!(sig_path, sig_path_for(&binary));

        let sig = std::fs::read(&sig_path).unwrap();
        let vk = VerifyingKey::from_bytes(&TEST_PUBLIC).unwrap();
        let sig = ed25519_dalek::Signature::from_slice(&sig).unwrap();
        assert!(vk.verify(b"macho/elf payload bytes", &sig).is_ok());
        assert!(vk.verify(b"tampered", &sig).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sig_path_appends_not_replaces() {
        assert_eq!(
            sig_path_for(Path::new("/x/veld-helper")),
            PathBuf::from("/x/veld-helper.sig")
        );
    }

    /// `Label(l)` / `Unquotable` / `Missing` as a short string, for asserting.
    fn header_of(input: &str) -> String {
        match pem_header(input) {
            PemHeader::Label(l) => l.to_string(),
            PemHeader::Unquotable => "<unquotable>".into(),
            PemHeader::Indented => "<indented>".into(),
            PemHeader::ByteOrderMark => "<bom>".into(),
            PemHeader::Missing => "<missing>".into(),
        }
    }

    /// A legitimately-PEM value whose label this tool will not echo must still
    /// be reported as *having* a header. Telling the operator there is no PEM
    /// header in a file that plainly has one is the same wrong-file-reported-as-
    /// something-else failure #339 exists to remove, pointed the other way.
    #[test]
    fn an_unquotable_label_is_reported_as_present_not_absent() {
        let pem =
            "-----BEGIN OpenVPN Static key V1-----\nabcdef\n-----END OpenVPN Static key V1-----\n";
        let err = parse_signing_key(pem, &env_source()).unwrap_err().render();
        assert!(
            err.contains("got a PEM block with an unrecognised label"),
            "{err}"
        );
        assert!(!err.contains("no PEM header at all"), "{err}");
        assert!(
            !err.contains("OpenVPN"),
            "the label must not be echoed: {err}"
        );
    }

    #[test]
    fn pem_header_reads_the_first_block_and_rejects_non_labels() {
        assert_eq!(header_of(TEST_PRIVATE_PEM), "PRIVATE KEY");
        assert_eq!(
            header_of("-----BEGIN OPENSSH PRIVATE KEY-----\nbody\n"),
            "OPENSSH PRIVATE KEY"
        );
        // Leading noise before the block is still a readable label.
        assert_eq!(
            header_of("# comment\n\n-----BEGIN PUBLIC KEY-----\nbody\n"),
            "PUBLIC KEY"
        );
        assert_eq!(header_of(""), "<missing>");
        assert_eq!(header_of("MC4CAQAwBQYDK2Vw"), "<missing>");
        // A "label" that is really a slice of some other file never escapes:
        // lowercase and punctuation are rejected, and so is anything longer
        // than a real label. RFC 7468 permits all of these as labels, so they
        // are reported as a header that is present but unquotable — never as
        // no header at all.
        assert_eq!(header_of("-----BEGIN -----"), "<unquotable>");
        assert_eq!(
            header_of("-----BEGIN secret/value+here-----"),
            "<unquotable>"
        );
        assert_eq!(
            header_of("-----BEGIN OpenVPN Static key V1-----"),
            "<unquotable>"
        );
        assert_eq!(
            header_of(&format!(
                "-----BEGIN {}-----",
                "A".repeat(MAX_LABEL_LEN + 1)
            )),
            "<unquotable>"
        );
    }

    #[test]
    fn a_public_key_names_the_label_found_expected_and_source() {
        let pem = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAVxMGvCvahvk4Vafq2qchdBFnCWzqtwMR0vfUMwMK8Mc=\n-----END PUBLIC KEY-----\n";
        let err = parse_signing_key(pem, &env_source()).unwrap_err().render();
        assert!(err.contains("got \"BEGIN PUBLIC KEY\""), "{err}");
        assert!(err.contains("\"BEGIN PRIVATE KEY\""), "{err}");
        assert!(err.contains("--key-env SIGNING_PRIVATE_KEY"), "{err}");
        assert!(err.contains("this looks like the .pub"), "{err}");
    }

    #[test]
    fn an_openssh_key_is_told_how_to_convert() {
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
        let err = parse_signing_key(pem, &env_source()).unwrap_err().render();
        assert!(err.contains("got \"BEGIN OPENSSH PRIVATE KEY\""), "{err}");
        assert!(err.contains("ssh-keygen -p -m PKCS8 -f <key>"), "{err}");
    }

    #[test]
    fn an_encrypted_key_is_told_to_decrypt_first() {
        let pem = "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIGbMFcGCSqGSIb3DQEFDT\n-----END ENCRYPTED PRIVATE KEY-----\n";
        let err = parse_signing_key(pem, &env_source()).unwrap_err().render();
        assert!(err.contains("got \"BEGIN ENCRYPTED PRIVATE KEY\""), "{err}");
        assert!(err.contains("decrypt it first"), "{err}");
    }

    #[test]
    fn an_unrecognised_label_still_names_what_it_got() {
        let pem = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
        let err = parse_signing_key(pem, &env_source()).unwrap_err().render();
        assert!(err.contains("got \"BEGIN CERTIFICATE\""), "{err}");
        assert!(!err.contains("hint:"), "{err}");
    }

    #[test]
    fn an_empty_value_is_diagnosed_as_empty_per_source() {
        // The v16.58.1-adjacent case: the secret simply never reached the job.
        let err = parse_signing_key("", &env_source()).unwrap_err().render();
        assert!(err.contains("got an empty value"), "{err}");
        assert!(err.contains("--key-env SIGNING_PRIVATE_KEY"), "{err}");
        assert!(err.contains("unset"), "{err}");

        let file = KeySource::File(PathBuf::from("/etc/veld/signing.pem"));
        let err = parse_signing_key("   \n\n", &file).unwrap_err().render();
        assert!(err.contains("got an empty value"), "{err}");
        // Narrowed to the filename — see `echoable_allows_real_names_paths_and_flags`.
        assert!(err.contains("--key-file …/signing.pem"), "{err}");
        assert!(err.contains("the file is empty"), "{err}");
    }

    #[test]
    fn a_value_with_no_pem_header_says_so() {
        let err = parse_signing_key("MC4CAQAwBQYDK2Vw\n", &env_source())
            .unwrap_err()
            .render();
        assert!(err.contains("found no PEM header at all"), "{err}");
        assert!(err.contains("-----BEGIN PRIVATE KEY-----"), "{err}");
    }

    #[test]
    fn a_correctly_labelled_but_broken_body_is_not_reported_as_the_wrong_file() {
        let body = key_body();
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            &body[..body.len() - 8]
        );
        let err = parse_signing_key(&pem, &env_source()).unwrap_err().render();
        assert!(err.contains("invalid ed25519 private key"), "{err}");
        assert!(err.contains("truncated or corrupt"), "{err}");
        // The upstream renderer would have said `expected 48, actual 30` here.
        assert!(!err.contains("0x") && !err.contains("48"), "{err}");
    }

    /// The whole private key uploaded, but the wrong one: a PKCS#8 key for a
    /// different algorithm. The right label, valid DER, and still unusable —
    /// so "corrupt or truncated" would send the operator hunting a transfer
    /// problem that does not exist.
    ///
    /// The fixture is `TEST_PRIVATE_PEM` with one byte of its algorithm OID
    /// changed (1.3.101.112 Ed25519 → 1.3.101.110 X25519), so no second key is
    /// committed here. A real RSA PKCS#8 key produces the same message.
    #[test]
    fn a_pkcs8_key_for_another_algorithm_says_so() {
        let x25519 = "-----BEGIN PRIVATE KEY-----\n\
            MC4CAQAwBQYDK2VuBCIEIG6yQcLcN3khrsV3dAHJmX/loSUSEoU9FVYNd4mqV+S1\n\
            -----END PRIVATE KEY-----\n";
        let err = parse_signing_key(x25519, &env_source())
            .unwrap_err()
            .render();
        assert!(err.contains("names a different algorithm"), "{err}");
        assert!(!err.contains("truncated"), "{err}");
    }

    /// A byte-perfect key whose boundary lines are indented — a key pasted into
    /// a YAML block scalar or an indented heredoc. `from_pkcs8_pem` rejects the
    /// document because RFC 7468 boundaries are anchored, and before this branch
    /// existed the operator was told the *body* was "truncated or corrupt" and
    /// that "the PEM header is right". Both were false, which is the #339 bug
    /// wearing a different hat.
    #[test]
    fn an_indented_pem_blames_the_indentation_not_the_body() {
        let indented: String = TEST_PRIVATE_PEM
            .lines()
            .map(|l| format!("  {l}\n"))
            .collect();
        assert_eq!(header_of(&indented), "<indented>");
        let err = parse_signing_key(&indented, &env_source())
            .unwrap_err()
            .render();
        assert!(err.contains("indented line"), "{err}");
        assert!(err.contains("column 0"), "{err}");
        assert!(!err.contains("corrupt"), "{err}");
        assert!(!err.contains("truncated"), "{err}");
    }

    /// A key saved by an editor that writes a UTF-8 BOM. `U+FEFF` is not
    /// `char::is_whitespace`, so the first line stops looking like a boundary —
    /// and the old message told the operator the value must start with a line
    /// it already started with, byte for byte.
    #[test]
    fn a_byte_order_mark_is_named_rather_than_reported_as_no_header() {
        let bom = format!("{}{TEST_PRIVATE_PEM}", '\u{feff}');
        assert_eq!(header_of(&bom), "<bom>");
        let err = parse_signing_key(&bom, &env_source()).unwrap_err().render();
        assert!(err.contains("byte-order mark"), "{err}");
        assert!(!err.contains("no PEM header at all"), "{err}");
    }

    /// Indentation is only the headline once the label is the one we want; a
    /// wrong label is the more useful thing to say, indented or not.
    #[test]
    fn a_wrong_label_wins_over_indentation() {
        let err = parse_signing_key("  -----BEGIN PUBLIC KEY-----\n  body\n", &env_source())
            .unwrap_err()
            .render();
        assert!(err.contains("got \"BEGIN PUBLIC KEY\""), "{err}");
        assert!(!err.contains("indented"), "{err}");
    }

    /// The three renderings a path can get, and when.
    ///
    /// Narrowing to `…/<filename>` is the common case, and it is a deliberate
    /// trade: the directory is dropped because no rule can tell a directory
    /// name from encoded bytes without being defeated by some encoding, and a
    /// key chunked into short pieces looks exactly like a directory tree. What
    /// the operator acts on — the flag, and which file — survives.
    #[test]
    fn echoable_allows_real_names_paths_and_flags() {
        assert_eq!(echoable_name("SIGNING_PRIVATE_KEY"), "SIGNING_PRIVATE_KEY");
        assert_eq!(echoable_flag("--kye-env"), "--kye-env");

        // Whole value: every segment carries a `.`, `-` or `_`.
        for path in ["signing.pem", "veld-helper", "id_ed25519/veld-helper"] {
            assert_eq!(echoable_path(path), path, "should echo whole: {path}");
        }

        // Narrowed to the filename: some directory above it is separator-free.
        for (path, shown) in [
            ("dist/veld-helper", "…/veld-helper"),
            ("/etc/veld/signing.pem", "…/signing.pem"),
            (
                "/Users/runner/work/veld/veld/dist/veld-helper",
                "…/veld-helper",
            ),
            (
                "/var/folders/0j/ccvfzg7j28l480jq1ydvmm340000gn/T/veld-helper",
                "…/veld-helper",
            ),
        ] {
            assert_eq!(echoable_path(path), shown, "should narrow: {path}");
        }

        // Redacted: nothing in it is a filename. The last is a key chunked to
        // look like a directory tree, which an earlier rule echoed in full.
        for path in [
            "MC4CAQAwBQYDK2VwBCIEIG6yQcLcN3khrsV3dAHJmX/loSUSEoU9FVYNd4mqV1S1",
            "3ef98423/c9fc3f18/b0cada12/7a41493a/106a9273/37ed5654/8f3ef07c",
        ] {
            assert_eq!(echoable_path(path), REDACTED, "should redact: {path}");
        }
    }

    /// Every encoding of the key, through every one of the three functions.
    ///
    /// The assertions use the bare body deliberately: an earlier version of
    /// this test used `key_body().repeat(4)` — 256 characters, just over a
    /// 200-character bound — and so passed while the real 64-character body
    /// leaked. Base32 is here because it is single-case, which is what defeated
    /// the version of the rule that looked for mixed case.
    #[test]
    fn echoable_redacts_the_key_in_every_encoding_it_travels_in() {
        let body = key_body();
        assert_eq!(body.len(), 64, "the fixture must be the real body length");
        let hex: String = "302e020100300506032b657004220420".repeat(3);

        let seed_bytes: Vec<u8> = (0u8..48).collect();
        let mut encoded_forms = vec![body.clone(), hex, TEST_PRIVATE_PEM.to_string()];
        encoded_forms.push(base32(&seed_bytes));
        encoded_forms.push(base32(&seed_bytes).to_lowercase());
        for encoded in &encoded_forms {
            assert_eq!(echoable_name(encoded), REDACTED, "name: {encoded}");
            assert_eq!(echoable_path(encoded), REDACTED, "path: {encoded}");
            assert_eq!(echoable_flag(encoded), REDACTED, "flag: {encoded}");
        }
        // A body split by base64's own `/` is still one long run per part.
        assert_eq!(
            echoable_path("MC4CAQAwBQYDK2VwBCIEIG6yQcLcN3khrsV3dAHJmX/loSUS"),
            REDACTED
        );
        assert_eq!(echoable_path("two\nlines"), REDACTED);
    }

    /// A re-indent that caught the body and the END line but not BEGIN is still
    /// rejected by the parser, and used to be blamed on the body.
    #[test]
    fn an_indented_body_counts_as_indentation_too() {
        let mut lines = TEST_PRIVATE_PEM.lines();
        let first = lines.next().unwrap();
        let rest: String = lines.map(|l| format!("  {l}\n")).collect();
        let pem = format!("{first}\n{rest}");
        assert_eq!(header_of(&pem), "<indented>");
        let err = parse_signing_key(&pem, &env_source()).unwrap_err().render();
        assert!(err.contains("indented line"), "{err}");
        assert!(!err.contains("corrupt"), "{err}");
    }

    /// Indented *and* padded: the hint must cover both, or an operator who does
    /// exactly what it says fails again on the other half.
    #[test]
    fn indentation_outranks_stray_whitespace_in_the_label() {
        let pem = "  -----BEGIN PRIVATE KEY -----\n  body\n  -----END PRIVATE KEY-----\n";
        assert_eq!(header_of(pem), "<indented>");
        let err = parse_signing_key(pem, &env_source()).unwrap_err().render();
        assert!(err.contains("column 0"), "{err}");
        assert!(err.contains("no extra spaces"), "{err}");
    }

    /// RFC 4648 base32 of `bytes` — uppercase, and single-case by spec, which
    /// is exactly what defeated the alphabet-based version of the argv guard.
    fn base32(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
        let mut out = String::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for &b in bytes {
            acc = (acc << 8) | u32::from(b);
            bits += 8;
            while bits >= 5 {
                bits -= 5;
                out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
            }
        }
        if bits > 0 {
            out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
        }
        out
    }

    /// Every encoding a mangled secret plausibly arrives in.
    fn encodings_of(seed: &[u8], pem: &str) -> Vec<String> {
        vec![
            pem.to_string(),
            pem.lines()
                .filter(|l| !l.trim_start().starts_with("-----"))
                .map(str::trim)
                .collect(),
            seed.iter().map(|b| format!("{b:02x}")).collect(),
            seed.iter().map(|b| format!("{b:02X}")).collect(),
            base32(seed),
            base32(seed).to_lowercase(),
        ]
    }

    /// Split `value` every `width` characters with `/`.
    ///
    /// A key body already contains `/` sometimes — base64's own alphabet — and
    /// where those land decides whether every segment is short enough to look
    /// like a path. Leaving that to chance is how the first version of this
    /// sweep passed against a guard that leaked: it needed a slash to fall in a
    /// particular window and usually none did. Chunking deliberately puts the
    /// adversarial shape in every iteration instead of ~10% of them.
    fn chunked(value: &str, width: usize) -> String {
        value
            .as_bytes()
            .chunks(width)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Hand-picked fixtures are how the argv guard got this wrong four times:
    /// base64 contains `/` so "has a slash" proved nothing; the key body is
    /// shorter than the "too long" bound; base32 is single-case so an
    /// alphabet-mixing test missed it. Every one of those rules judged a
    /// *property of the key* and was beaten by an encoding that lacked it.
    ///
    /// The rule now judges what a filename has — a `.`, `-` or `_`, which no
    /// base64, base32 or hex alphabet contains — and this sweep is what says so
    /// out loud: many real random keys, in six encodings, through every argv
    /// position, with no eight-character run of the body surviving.
    #[test]
    fn no_random_key_survives_any_argv_position_in_any_encoding() {
        for _ in 0..128 {
            let mut seed = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .expect("open /dev/urandom")
                .read_exact(&mut seed)
                .expect("read seed");
            let key = ed25519_dalek::SigningKey::from_bytes(&seed);
            let pem = key
                .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
                .expect("encode PKCS#8 PEM");
            let body: String = pem
                .lines()
                .filter(|l| !l.trim_start().starts_with("-----"))
                .map(str::trim)
                .collect();

            let mut forms = encodings_of(&seed, &pem);
            // Every encoding again as a path whose segments are all short
            // enough to pass for filenames.
            for width in [8, 20, 30, 39] {
                for form in encodings_of(&seed, &pem) {
                    forms.push(chunked(&form, width));
                }
            }
            for encoding in forms {
                let messages = [
                    KeySource::Env(encoding.clone()).to_string(),
                    KeySource::File(PathBuf::from(&encoding)).to_string(),
                    format!("unknown flag: {}", echoable_flag(&encoding)),
                ];
                for message in messages {
                    for window in encoding.as_bytes().windows(8) {
                        let needle = std::str::from_utf8(window).unwrap();
                        assert!(
                            !message.contains(needle),
                            "argv leaked key material {needle:?}:\n{message}"
                        );
                    }
                    for window in body.as_bytes().windows(8) {
                        let needle = std::str::from_utf8(window).unwrap();
                        assert!(!message.contains(needle), "argv leaked body:\n{message}");
                    }
                }
            }
        }
    }

    /// The fact the residual rests on, checked rather than asserted in prose.
    ///
    /// `echoable_path`'s narrowing could surface one segment of a key only if a
    /// key encoding supplied **both** a `/` (to split on) and a `.`, `-` or `_`
    /// (to pass for a filename). No encoding does: standard base64 has the
    /// slash and none of the others; URL-safe base64 has `-` and `_` and never
    /// a slash; base32 and hex have neither. So reaching that residual takes a
    /// separator placed by hand, which is not the pasted-secret accident this
    /// tool exists to diagnose. If a future encoding is added to the sweep and
    /// this stops holding, the residual stops being theoretical — hence a test.
    #[test]
    fn no_encoding_supplies_both_a_slash_and_a_name_character() {
        for _ in 0..64 {
            let mut seed = [0u8; 48];
            std::fs::File::open("/dev/urandom")
                .expect("open /dev/urandom")
                .read_exact(&mut seed)
                .expect("read seed");

            let encodings = [
                BASE64_STANDARD.encode(seed),
                BASE64_URL_SAFE_NO_PAD.encode(seed),
                base32(&seed),
                base32(&seed).to_lowercase(),
                seed.iter().map(|b| format!("{b:02x}")).collect(),
            ];

            for encoded in encodings {
                let has_slash = encoded.contains('/');
                let has_name_char = encoded.chars().any(|c| matches!(c, '.' | '-' | '_'));
                assert!(
                    !(has_slash && has_name_char),
                    "an encoding supplies both a separator and a filename \
                     character, so a split of it could pass as a path: {encoded}"
                );
            }
        }

        // A whole PEM does carry both — `/` in its base64 body and `-` in its
        // boundary lines — and is disqualified one step earlier instead: its
        // newlines are not in the segment charset at all, so no part of a PEM
        // is ever a printable segment. Asserting that here keeps the two halves
        // of the argument in one place.
        assert!(TEST_PRIVATE_PEM.contains('/') || TEST_PRIVATE_PEM.contains('-'));
        assert!(TEST_PRIVATE_PEM.chars().any(|c| c.is_control()));
        assert_eq!(echoable_path(TEST_PRIVATE_PEM), REDACTED);
        for segment in TEST_PRIVATE_PEM.split('/') {
            assert!(
                !is_named_segment(segment),
                "a slice of a PEM passed as a filename: {segment}"
            );
        }
    }

    /// URL-safe base64 is the one encoding whose alphabet contains the `.`/`-`/`_`
    /// that the segment rule keys on, so it gets its own test rather than being
    /// folded into the sweep — the guarantee for it is genuinely weaker, and a
    /// test that quietly asserted the strong property for it would be lying.
    ///
    /// Whole, it is far past [`MAX_NAME_LEN`] and vanishes. Split on `/` into
    /// filename-sized pieces, the rule bounds what can surface to a single
    /// segment of at most [`MAX_SEGMENT_ALNUM`] alphanumerics — never the key,
    /// and never more than one piece of it. This test pins that bound, so
    /// widening the cap fails here rather than in a log.
    #[test]
    fn url_safe_base64_is_bounded_even_when_chunked_to_look_like_a_path() {
        for _ in 0..64 {
            let mut seed = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .expect("open /dev/urandom")
                .read_exact(&mut seed)
                .expect("read seed");
            let encoded = BASE64_URL_SAFE_NO_PAD.encode(seed);

            // Whole: nothing of it survives.
            assert_eq!(echoable_path(&encoded), REDACTED);
            assert_eq!(echoable_name(&encoded), REDACTED);
            assert_eq!(echoable_flag(&encoded), REDACTED);

            for width in [8, 20, 30, 39] {
                let path = chunked(&encoded, width);
                let shown = echoable_path(&path).into_owned();
                if shown == REDACTED {
                    continue; // the best outcome, and the common one
                }
                assert!(!shown.contains(&encoded), "the whole key surfaced: {shown}");
                // Whatever is shown is one bounded segment, not a reassembly.
                let alnum = shown.chars().filter(|c| c.is_ascii_alphanumeric()).count();
                assert!(
                    alnum <= MAX_SEGMENT_ALNUM,
                    "more than one segment surfaced ({alnum} alphanumerics): {shown}"
                );
            }
        }
    }

    /// The security half of #339. Errors may name the PEM label and the source,
    /// and nothing else — no key body, no prefix of it, no length that narrows
    /// it. `veld-sign` runs in CI with the org private key in its environment
    /// and its stderr goes straight into a public workflow log.
    #[test]
    fn key_body_never_appears_in_an_error() {
        let body = key_body();
        assert!(body.len() > 32, "test key body looks wrong: {}", body.len());

        let inputs = [
            // The real key under a correct header, truncated so the parser
            // rejects it: the one failure path handed genuine key material,
            // and the only one that could leak it through an upstream error.
            format!(
                "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
                &body[..body.len() - 8]
            ),
            // The real key under every wrong header, so the label branch is
            // covered with material that actually matters.
            format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----\n"),
            format!(
                "-----BEGIN OPENSSH PRIVATE KEY-----\n{body}\n-----END OPENSSH PRIVATE KEY-----\n"
            ),
            format!("-----BEGIN veld/private+key-----\n{body}\n-----END veld/private+key-----\n"),
            // Bare key material with no header at all.
            body.clone(),
        ];
        let sources = [
            env_source(),
            KeySource::File(PathBuf::from("/etc/veld/signing.pem")),
        ];

        for source in &sources {
            for input in &inputs {
                let rendered = match parse_signing_key(input, source) {
                    Ok(_) => panic!("expected {input:?} to be rejected"),
                    Err(e) => e.render(),
                };
                for window in body.as_bytes().windows(8) {
                    let needle = std::str::from_utf8(window).unwrap();
                    assert!(
                        !rendered.contains(needle),
                        "error leaked key material {needle:?}:\n{rendered}"
                    );
                }
                assert!(
                    !rendered.contains(&body.len().to_string()),
                    "error leaked the key length:\n{rendered}"
                );
                // The window scan above is necessary but not sufficient, and
                // for a while it was the only check: it looks for runs of the
                // *base64* body, so it was blind to the one leak that actually
                // existed. `der` renders a rejected byte as `0x11` and a short
                // body as `expected 48, actual 30` — neither is a substring of
                // the base64, and both disclose the key. Nothing derived from
                // the body may reach stderr in any encoding, so: no hex, and no
                // digits beyond the two that live in fixed words.
                assert!(
                    !rendered.contains("0x"),
                    "error rendered a raw byte of the key:\n{rendered}"
                );
                // The only digits any message may contain belong to these
                // fixed words. Adding a message with a number in it means
                // adding it here, which is the point: a number in this output
                // is far more likely to have come from the key than from
                // prose.
                const DIGITS_IN_FIXED_WORDS: [&str; 3] = ["PKCS#8", "PKCS8", "ed25519"];
                let mut residue = rendered.clone();
                for word in DIGITS_IN_FIXED_WORDS {
                    residue = residue.replace(word, "");
                }
                assert!(
                    !residue.chars().any(|c| c.is_ascii_digit()),
                    "error carries a number derived from the key (a length, an \
                     offset, a byte):\n{rendered}"
                );
            }
        }
    }
}
