//! End-to-end smoke test for the release signing path (issue #339).
//!
//! `release.yml`'s `Package client binaries` step is the only place `veld-sign`
//! ever runs in production, and it runs **only on a tagged release**. Nothing on
//! a PR or a merge to main used to touch this tool, its key handling, or the
//! macOS ad-hoc-then-ed25519 ordering — so the first execution of a change here
//! was the release itself. That is how v16.58.1 failed at packaging, and it is
//! the same blind spot that hid the `veld-sign` cross-compile bug found in
//! #253's review: CI's `Release Build` job compiles, it never packages.
//!
//! So this drives the real binary exactly as that step drives it: through the
//! process boundary (`--key-env` / `--key-file`, exit status, stderr), not
//! through the library.
//!
//! Every key here is generated per run from `/dev/urandom` and lives only in a
//! temp dir. The org key is never involved and `SIGNING_PRIVATE_KEY` is never
//! read — the CI job that runs this has no access to the secret at all.

use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::pkcs8::spki::EncodePublicKey;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;

// Verification goes through veld-core deliberately. `verify_data` is the exact
// primitive the root helper's fail-closed relaunch gate calls — including its
// own 64-byte length check and `verify_strict` — and `sig_path_for` is the rule
// it uses to find the `.sig` beside a binary. Using them here, rather than a
// second hand-rolled ed25519 check and a third copy of the path rule, is what
// makes a drift between what veld-sign writes and what veld-helper will accept
// fail a PR instead of a release.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use veld_core::signing::{sig_path_for, verify_data};

/// The env var the smoke test hands keys through. Deliberately **not**
/// `SIGNING_PRIVATE_KEY`: nothing on a PR-triggered path may name the secret,
/// so a copy-paste from here can never reach the real one.
const KEY_ENV: &str = "VELD_SIGN_SMOKE_KEY";

/// `veld-sign`'s documented exit codes. `release.yml` runs the tool under a
/// `bash -e` step, so it only distinguishes zero from non-zero — but the split
/// is what an operator reads to tell "you invoked me wrong" from "the key you
/// gave me is not usable", and nothing else pins it.
const EXIT_BAD_INPUT: i32 = 1;
const EXIT_USAGE: i32 = 2;

/// A fresh throwaway key. Any 32 bytes are a valid ed25519 seed, so this needs
/// no RNG crate — and `/dev/urandom` keeps it genuinely per-run rather than a
/// constant committed next to a tool that signs root binaries.
fn throwaway_key() -> SigningKey {
    let mut seed = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .expect("open /dev/urandom")
        .read_exact(&mut seed)
        .expect("read seed");
    SigningKey::from_bytes(&seed)
}

/// A private, empty scratch dir named after the test that owns it (tests run in
/// parallel in one process, so the pid alone would collide).
///
/// Returned as a guard so the directory is removed on unwind too. Every test
/// here writes a throwaway private key into it, and an assertion that fails
/// part-way through would otherwise leave that key behind at a fully
/// predictable path. It is only ever a per-run `/dev/urandom` key that signs a
/// dummy payload — never the org key — but a signing tool's test suite is the
/// last place that should be casual about it.
///
/// `temp_dir()` is `$TMPDIR` (per-user) on macOS and `/tmp` (shared, sticky) on
/// Linux, so the mode is set explicitly rather than left to the umask.
struct Scratch(PathBuf);

impl Scratch {
    fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("veld-sign-smoke-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .expect("restrict scratch dir");
    Scratch(dir)
}

fn write_pkcs8_pem(key: &SigningKey, path: &Path) {
    let pem = key.to_pkcs8_pem(LineEnding::LF).expect("encode PKCS#8 PEM");
    std::fs::write(path, pem.as_bytes()).expect("write key file");
}

fn veld_sign() -> Command {
    Command::new(env!("CARGO_BIN_EXE_veld-sign"))
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Run `veld-sign` over `binary` with the key supplied through the env var —
/// the shape `release.yml` actually uses. `None` removes the variable entirely,
/// which is what a missing secret looks like to the job.
fn sign_with_env(key_pem: Option<&str>, binary: &Path) -> Output {
    let mut cmd = veld_sign();
    cmd.args(["--key-env", KEY_ENV]).arg(binary);
    match key_pem {
        Some(pem) => cmd.env(KEY_ENV, pem),
        None => cmd.env_remove(KEY_ENV),
    };
    cmd.output().expect("run veld-sign")
}

/// Run `veld-sign` over `binary` with the key in a file on disk.
fn sign_with_file(key_path: &Path, binary: &Path) -> Output {
    veld_sign()
        .arg("--key-file")
        .arg(key_path)
        .arg(binary)
        .output()
        .expect("run veld-sign")
}

/// The happy path, over both key sources: a detached signature is written, it
/// is exactly 64 raw bytes, and it verifies against the throwaway public key —
/// and only against the bytes that were actually signed.
#[test]
fn signs_a_dummy_file_with_a_64_byte_verifiable_signature() {
    let dir = scratch("happy");
    let key = throwaway_key();
    let key_path = dir.join("signing.pem");
    write_pkcs8_pem(&key, &key_path);
    let key_pem = std::fs::read_to_string(&key_path).unwrap();

    for source in ["--key-file", "--key-env"] {
        let binary = dir.join(format!("veld-helper{source}"));
        let payload = format!("macho/elf payload bytes for {source}");
        std::fs::write(&binary, &payload).unwrap();

        let out = if source == "--key-file" {
            sign_with_file(&key_path, &binary)
        } else {
            sign_with_env(Some(&key_pem), &binary)
        };
        assert!(
            out.status.success(),
            "{source} signing failed: {}",
            stderr_of(&out)
        );

        // `sig_path_for` is veld-core's own rule, so this read failing means
        // the writer and the reader disagree about where the `.sig` lives.
        let sig_bytes =
            std::fs::read(sig_path_for(&binary)).expect("signature written next to the binary");
        assert_eq!(
            sig_bytes.len(),
            64,
            "{source}: the detached signature must be the raw 64-byte ed25519 \
             signature — veld-core's verifier rejects any other length outright"
        );

        let pubkey = key.verifying_key().to_bytes();
        assert!(
            verify_data(&pubkey, payload.as_bytes(), &sig_bytes),
            "{source}: the signature veld-sign wrote is not one veld-core accepts"
        );
        assert!(
            !verify_data(&pubkey, b"tampered", &sig_bytes),
            "{source}: signature verified against bytes it does not cover"
        );
    }
}

/// The failure that actually broke the v16.58.1 release: the secret held the
/// sibling `.pub` file. The message must name what it got, what it wanted, and
/// which source it read — and must not carry the key.
#[test]
fn rejects_a_public_key_naming_the_label_and_the_source() {
    let dir = scratch("pubkey");
    let key = throwaway_key();
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    let public_pem = key
        .verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("encode SPKI PEM");

    let out = sign_with_env(Some(&public_pem), &binary);
    assert_eq!(
        out.status.code(),
        Some(EXIT_BAD_INPUT),
        "a public key must be rejected as bad input, not as a usage error: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(err.contains("got \"BEGIN PUBLIC KEY\""), "{err}");
    assert!(err.contains("\"BEGIN PRIVATE KEY\""), "{err}");
    assert!(err.contains(&format!("--key-env {KEY_ENV}")), "{err}");
    assert!(err.contains("this looks like the .pub"), "{err}");
    assert!(
        !sig_path_for(&binary).exists(),
        "a rejected key must not leave a signature behind"
    );
    assert_no_key_material(&err, &public_pem);
}

/// An OpenSSH-format private key, from `ssh-keygen` when it is available.
///
/// The fallback matters: `cargo test --workspace` runs this file inside `check`
/// — the ubuntu job every other job `needs:` — so an `ssh-keygen` missing from
/// some future runner image would red out all of CI for a reason unrelated to
/// the diff under test. A few lines above its own test step, `check` installs
/// zsh explicitly so that a runner image cannot decide whether a test runs;
/// this answers the same concern without a second apt package. The real tool
/// stays preferred, so the label asserted below is the one OpenSSH actually
/// writes if that ever changes.
///
/// The fixture's body is deliberately not a key — only the label is under test,
/// and `veld-sign` rejects the file on its label before reading any further.
fn openssh_format_key(dir: &Scratch) -> PathBuf {
    const FIXTURE: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\n\
        bm90IGEga2V5IC0tIHRoZSBsYWJlbCBpcyB3aGF0IHRoaXMgZml4dHVyZSBpcyBmb3I=\n\
        -----END OPENSSH PRIVATE KEY-----\n";

    let key_path = dir.join("id_ed25519");
    let generated = Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-q", "-f"])
        .arg(&key_path)
        .output();
    match generated {
        Ok(out) if out.status.success() => key_path,
        _ => {
            std::fs::write(&key_path, FIXTURE).expect("write OpenSSH fixture");
            key_path
        }
    }
}

/// The other wrong-file shape a maintainer reaches for: an SSH key, in the
/// format `ssh-keygen` writes rather than the PKCS#8 this tool wants.
#[test]
fn rejects_an_openssh_key_with_a_conversion_hint() {
    let dir = scratch("openssh");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    let key_path = openssh_format_key(&dir);

    let out = sign_with_file(&key_path, &binary);
    assert_eq!(
        out.status.code(),
        Some(EXIT_BAD_INPUT),
        "an OpenSSH key must be rejected as bad input: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(err.contains("got \"BEGIN OPENSSH PRIVATE KEY\""), "{err}");
    assert!(err.contains("ssh-keygen -p -m PKCS8 -f <key>"), "{err}");
    // The file's own name, not necessarily the whole path: under macOS's
    // `$TMPDIR` the random directory component is indistinguishable from
    // encoded bytes, so `veld-sign` narrows the rendering to `…/id_ed25519`.
    // Naming the file is the part the operator needs.
    assert!(
        err.contains("id_ed25519"),
        "the message must name the file to go and look at:\n{err}"
    );
    assert!(!sig_path_for(&binary).exists());
    assert_no_key_material(&err, &std::fs::read_to_string(&key_path).unwrap());
}

/// A secret that never reached the job at all — absent, empty, or whitespace.
/// All three are one operator mistake and get one message.
#[test]
fn rejects_an_empty_or_unset_secret() {
    let dir = scratch("empty");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    for key_pem in [None, Some(""), Some("   \n\n")] {
        let out = sign_with_env(key_pem, &binary);
        assert_eq!(
            out.status.code(),
            Some(EXIT_BAD_INPUT),
            "an empty key must be rejected as bad input ({key_pem:?}): {}",
            stderr_of(&out)
        );
        let err = stderr_of(&out);
        assert!(err.contains("got an empty value"), "{key_pem:?}: {err}");
        assert!(
            err.contains(&format!("--key-env {KEY_ENV}")),
            "{key_pem:?}: {err}"
        );
        assert!(err.contains("unset"), "{key_pem:?}: {err}");
    }
    assert!(!sig_path_for(&binary).exists());
}

/// The exit-code split, which is otherwise pinned nowhere: `EXIT_USAGE` means
/// the command line itself is wrong, before any input is touched;
/// `EXIT_BAD_INPUT` means the tool ran and could not produce a signature from
/// what it was given.
///
/// It drifted once already. Before #339 an unset `--key-env` variable exited 2,
/// reading as "you invoked me wrong" for what is really a missing secret — and
/// an unreadable `--key-file` still exited 2 for the same reason. Both are the
/// same mistake as an empty value and now exit 1 with the same vocabulary.
#[test]
fn separates_a_bad_invocation_from_bad_input() {
    let dir = scratch("usage");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();
    let key_path = dir.join("signing.pem");
    write_pkcs8_pem(&throwaway_key(), &key_path);

    for args in [
        vec![],                             // no key source, no binary
        vec!["--key-env".to_string()],      // flag with no value
        vec!["--key-file".to_string()],     // flag with no value
        vec![binary.display().to_string()], // binary but no key source
        vec!["--nonsense".to_string()],     // unknown flag
        // Two key sources at once — the shape of a half-edited release step.
        vec![
            "--key-env".to_string(),
            KEY_ENV.to_string(),
            "--key-file".to_string(),
            key_path.display().to_string(),
            binary.display().to_string(),
        ],
    ] {
        let out = veld_sign().args(&args).output().expect("run veld-sign");
        assert_eq!(
            out.status.code(),
            Some(EXIT_USAGE),
            "{args:?} is a usage error: {}",
            stderr_of(&out)
        );
        assert!(
            stderr_of(&out).contains("usage: veld-sign"),
            "{args:?} must print the usage line: {}",
            stderr_of(&out)
        );
    }

    // Bad *input*, not a bad invocation: the flags were right in both cases.
    for (what, out) in [
        (
            "a missing key file",
            sign_with_file(&dir.join("does-not-exist.pem"), &binary),
        ),
        (
            "a missing binary to sign",
            sign_with_file(&key_path, &dir.join("no-such-binary")),
        ),
    ] {
        assert_eq!(
            out.status.code(),
            Some(EXIT_BAD_INPUT),
            "{what} is bad input: {}",
            stderr_of(&out)
        );
        assert!(
            !stderr_of(&out).contains("usage: veld-sign"),
            "{what} is not a usage error: {}",
            stderr_of(&out)
        );
    }
}

/// `veld-sign` runs in CI with the org private key in its environment and its
/// stderr goes straight into a public workflow log, so no body line of the key
/// it was handed — nor any 8-character run of one — may appear in the output.
fn assert_no_key_material(stderr: &str, key_pem: &str) {
    let body: String = key_pem
        .lines()
        .filter(|l| !l.trim_start().starts_with("-----"))
        .map(str::trim)
        .collect();
    assert!(
        body.len() > 32,
        "fixture key body looks wrong: {}",
        body.len()
    );
    for window in body.as_bytes().windows(8) {
        let needle = std::str::from_utf8(window).unwrap();
        assert!(
            !stderr.contains(needle),
            "stderr leaked key material {needle:?}:\n{stderr}"
        );
    }
}

/// The leak this diff was reviewed into fixing, pinned at the process boundary.
///
/// A correctly-labelled body that is not valid PKCS#8 used to have the upstream
/// RustCrypto error interpolated straight into stderr, and `der` quotes its
/// input: a bare 32-byte seed pasted under a `-----BEGIN PRIVATE KEY-----`
/// header printed `unknown/unsupported ASN.1 DER tag: 0x11`, where `0x11` was
/// `seed[0]`; a truncated key printed `expected 48, actual 30`. Both went into
/// a public workflow log with the org key in the environment.
///
/// The seed here is the throwaway's own, so if this regresses the test is
/// leaking a key it generated rather than one that matters — and it fails.
#[test]
fn a_corrupt_body_is_diagnosed_without_quoting_any_of_it() {
    let dir = scratch("corrupt");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    // A bare seed under the right label: valid base64, not valid PKCS#8 DER.
    let seed = throwaway_key().to_bytes();
    let body = BASE64.encode(seed);
    let pem = format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----\n");

    let out = sign_with_env(Some(&pem), &binary);
    assert_eq!(
        out.status.code(),
        Some(EXIT_BAD_INPUT),
        "a non-PKCS#8 body must be rejected: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(err.contains("truncated or corrupt"), "{err}");
    assert_no_key_material(&err, &pem);
    assert!(
        !err.contains("0x"),
        "stderr rendered a raw byte of the key:\n{err}"
    );
    // No path is interpolated on the --key-env branch, so every digit left in
    // the message would have come from the key.
    let residue = err
        .replace("PKCS#8", "")
        .replace("PKCS8", "")
        .replace("ed25519", "");
    assert!(
        !residue.chars().any(|c| c.is_ascii_digit()),
        "stderr carries a number derived from the key (a length, an offset, a \
         byte):\n{err}"
    );
    assert!(!sig_path_for(&binary).exists());
}

/// The worst failure this tool can have, pinned at the process boundary.
///
/// `std::env::var`'s `VarError::NotUnicode` Debug-formats the whole value, so a
/// single non-UTF-8 byte anywhere in `SIGNING_PRIVATE_KEY` printed the **entire
/// private key** into the public Actions log — no attacker needed, just a
/// Latin-1 paste, a BOM, or raw DER uploaded instead of PEM. That is the very
/// situation this issue exists to diagnose, so the diagnosis path was the one
/// that leaked.
///
/// Nothing else can catch this: the unit tests call `parse_signing_key`
/// directly and never go through the environment, and `Command::env` is usually
/// handed a `&str`. This test sets genuinely invalid bytes with `OsStr::from_bytes`
/// and asserts the key does not come back out.
#[test]
fn a_non_utf8_secret_does_not_print_the_key() {
    let dir = scratch("notutf8");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    let key = throwaway_key();
    let pem = key.to_pkcs8_pem(LineEnding::LF).expect("encode PKCS#8 PEM");
    // One stray byte in the middle of the base64 body — the realistic shape,
    // and the one GitHub's secret masking cannot redact because the mangled
    // line no longer matches the registered secret.
    let mut raw = pem.as_bytes().to_vec();
    let midpoint = raw.len() / 2;
    raw.insert(midpoint, 0xff);

    let out = veld_sign()
        .args(["--key-env", KEY_ENV])
        .arg(&binary)
        .env(KEY_ENV, std::ffi::OsStr::from_bytes(&raw))
        .output()
        .expect("run veld-sign");

    assert_eq!(
        out.status.code(),
        Some(EXIT_BAD_INPUT),
        "a non-UTF-8 secret is bad input: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(err.contains("not valid UTF-8"), "{err}");
    assert!(err.contains(&format!("--key-env {KEY_ENV}")), "{err}");

    // The key must not appear in any encoding: not as text, and not as the
    // Debug-escaped rendering `VarError::NotUnicode` used to produce.
    assert_no_key_material(&err, &pem);
    let debug_rendered = format!("{:?}", String::from_utf8_lossy(&raw));
    for line in debug_rendered.split("\\n").filter(|l| !l.contains("-----")) {
        let line = line.trim_matches('"');
        if line.len() >= 8 {
            assert!(
                !err.contains(line),
                "stderr leaked the Debug rendering of the key:\n{err}"
            );
        }
    }
    assert!(!sig_path_for(&binary).exists());
}

/// The macOS ordering that `release.yml` and `install.sh` depend on, and that
/// nothing but a real install has ever exercised: ad-hoc `codesign` first, then
/// ed25519-sign the ad-hoc-signed bytes, then let install.sh re-sign ad-hoc on
/// the user's machine. The `.sig` must still verify afterwards, which requires
/// the re-sign to reproduce the signed bytes exactly.
///
/// **The trap.** An ad-hoc signature's identifier is derived from the file's
/// **basename**, and that identifier is hashed into the CodeDirectory. So both
/// signings must see the file under the same name, or the bytes differ for a
/// reason that has nothing to do with idempotency — which is why `release.yml`
/// signs `dist/veld-helper` rather than `target/<target>/release/veld-helper`,
/// and why this test signs and re-signs one path. Working that out cost an hour
/// during #253's review; it is written down here so nobody re-derives it.
///
/// The fixture is a copy of `veld-sign` itself because `codesign` only accepts
/// a real Mach-O — a text file is rejected outright, so it has to be a binary
/// that is already on disk and native to this host.
#[cfg(target_os = "macos")]
#[test]
fn adhoc_resign_after_ed25519_signing_is_byte_idempotent() {
    let dir = scratch("resign");
    let key = throwaway_key();
    let key_path = dir.join("signing.pem");
    write_pkcs8_pem(&key, &key_path);

    // Named `veld-helper` for the whole sequence: see the basename note above.
    let binary = dir.join("veld-helper");
    std::fs::copy(env!("CARGO_BIN_EXE_veld-sign"), &binary).expect("copy a real Mach-O");

    adhoc_sign(&binary);
    let signed_bytes = std::fs::read(&binary).unwrap();

    let out = sign_with_file(&key_path, &binary);
    assert!(out.status.success(), "signing failed: {}", stderr_of(&out));

    let sig_bytes = std::fs::read(sig_path_for(&binary)).unwrap();
    assert_eq!(sig_bytes.len(), 64);

    // install.sh's own `codesign --force --sign -`, as it runs on the machine
    // of whoever ran `curl | bash` or `veld update`.
    adhoc_sign(&binary);
    let resigned_bytes = std::fs::read(&binary).unwrap();

    assert_eq!(
        signed_bytes.len(),
        resigned_bytes.len(),
        "ad-hoc re-sign changed the binary's length, so install.sh invalidates \
         the .sig that CI shipped"
    );
    assert!(
        signed_bytes == resigned_bytes,
        "ad-hoc re-sign was not byte-idempotent, so install.sh invalidates the \
         .sig that CI shipped"
    );
    // Through veld-core, because the thing that must still hold is precisely
    // "the root helper will relaunch onto these bytes".
    assert!(
        verify_data(&key.verifying_key().to_bytes(), &resigned_bytes, &sig_bytes),
        "signature must still verify after install.sh's re-sign"
    );
}

#[cfg(target_os = "macos")]
fn adhoc_sign(path: &Path) {
    let out = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(path)
        .output()
        .expect("run codesign");
    assert!(
        out.status.success(),
        "codesign --force --sign - {}: {}",
        path.display(),
        stderr_of(&out)
    );
}
