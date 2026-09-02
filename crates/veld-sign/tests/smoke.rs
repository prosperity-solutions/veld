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
//!
//! # The multi-key half (#261 slice C)
//!
//! A key rotation ships as a release signed by several keys at once, and that
//! path runs in production **once**, on the worst day, under time pressure. The
//! release step it runs in is push-only, so a pull request cannot reach it. These
//! tests are therefore the only place the rotation format is ever exercised
//! before it is needed: they drive the real tool with two and three throwaway
//! keys and check the artifact against `veld_core`'s reader, plus against a
//! re-implementation of the *pre-rotation* reader, which is the one already
//! compiled into every helper in the field and the one that cannot be fixed.

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
use veld_core::signing::{SIG_SLOT_LEN, sig_path_for, verify_data, verify_data_slots};

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

/// Run `veld-sign` with several keys in files, in order — the shape a rotation
/// release has, with `--key-file` standing in for `--key-env` so the test does
/// not have to juggle process environments.
fn sign_with_files(key_paths: &[&Path], binary: &Path) -> Output {
    let mut cmd = veld_sign();
    for path in key_paths {
        cmd.arg("--key-file").arg(path);
    }
    cmd.arg(binary).output().expect("run veld-sign")
}

/// `n` throwaway keys written to `dir`, returned with their paths.
fn key_files(dir: &Scratch, n: usize) -> (Vec<SigningKey>, Vec<PathBuf>) {
    let mut keys = Vec::new();
    let mut paths = Vec::new();
    for i in 0..n {
        let key = throwaway_key();
        let path = dir.join(format!("signing-{i}.pem"));
        write_pkcs8_pem(&key, &path);
        keys.push(key);
        paths.push(path);
    }
    (keys, paths)
}

fn pubkey_hex(key: &SigningKey) -> String {
    key.verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Several keys produce one 64-byte slot each, in the order given, all over the
/// same bytes — and each slot verifies under its own key and no other.
#[test]
fn several_keys_write_one_slot_each_in_order() {
    let dir = scratch("slots");
    let (keys, paths) = key_files(&dir, 3);
    let binary = dir.join("veld-helper");
    let payload = b"macho/elf payload for a rotation release";
    std::fs::write(&binary, payload).unwrap();

    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let out = sign_with_files(&refs, &binary);
    assert!(out.status.success(), "signing failed: {}", stderr_of(&out));

    let sig = std::fs::read(sig_path_for(&binary)).expect("signature written");
    assert_eq!(
        sig.len(),
        3 * SIG_SLOT_LEN,
        "three keys must write three slots, not {} bytes",
        sig.len()
    );

    for (i, key) in keys.iter().enumerate() {
        let slot = &sig[i * SIG_SLOT_LEN..(i + 1) * SIG_SLOT_LEN];
        assert!(
            verify_data(&key.verifying_key().to_bytes(), payload, slot),
            "slot {i} is not signed by key {i}: the order of --key-file flags is \
             the slot order, and slot 0 is a compatibility contract"
        );
        // And a helper whose keyring holds only this one key accepts the whole
        // artifact — which is what "any slot, any trusted key" has to mean.
        assert!(verify_data_slots(
            &[key.verifying_key().to_bytes()],
            payload,
            &sig
        ));
    }

    // A key that never signed it is refused however many slots there are.
    let stranger = throwaway_key();
    assert!(!verify_data_slots(
        &[stranger.verifying_key().to_bytes()],
        payload,
        &sig
    ));
}

/// **The load-bearing compatibility check, through the real tool.**
///
/// A helper already in the field reads exactly bytes `0..64` of the `.sig` and
/// verifies them against its one embedded key. That code cannot be changed, so
/// this re-implements it and asserts a multi-slot artifact still satisfies it. If
/// this fails, the release it stands for is one every privileged install refuses
/// to relaunch onto, and sudo is the only repair left.
#[test]
fn a_pre_rotation_reader_accepts_a_multi_slot_signature() {
    let dir = scratch("preroll");
    let (keys, paths) = key_files(&dir, 3);
    let binary = dir.join("veld-helper");
    let payload = b"the release that carries the rotation";
    std::fs::write(&binary, payload).unwrap();

    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    assert!(sign_with_files(&refs, &binary).status.success());

    // Exactly the shipped reader: a fixed 64-byte buffer, `read_exact`, and
    // everything past it never read.
    let mut first_slot = [0u8; 64];
    std::fs::File::open(sig_path_for(&binary))
        .expect("open .sig")
        .read_exact(&mut first_slot)
        .expect("a release must always carry a whole first slot");

    assert!(
        verify_data(&keys[0].verifying_key().to_bytes(), payload, &first_slot),
        "the pre-rotation reader does not accept this artifact"
    );
}

/// A bad key in **any** slot writes no signature at all.
///
/// The failure this forbids is a partial `.sig`: signed with key 0, then key 1
/// turns out to be unusable, and a 64-byte file is left behind. Nothing
/// downstream can see that as an error — it is a perfectly valid signature for
/// one generation and a missing slot for every other, discovered a release later.
#[test]
fn a_bad_key_in_a_later_slot_writes_no_signature_at_all() {
    let dir = scratch("partial");
    let (_, paths) = key_files(&dir, 1);
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();
    let broken = dir.join("broken.pem");
    std::fs::write(
        &broken,
        "-----BEGIN PRIVATE KEY-----\nnot base64 at all\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();

    let out = sign_with_files(&[&paths[0], &broken], &binary);
    assert_eq!(
        out.status.code(),
        Some(EXIT_BAD_INPUT),
        "{}",
        stderr_of(&out)
    );
    assert!(
        !sig_path_for(&binary).exists(),
        "a failed multi-key signing left a partial .sig behind — that is a \
         release which verifies for one generation and not the others"
    );
    // Nor any staging file. The write goes through `<sig>.incoming.<pid>` and a
    // rename, so a truncated `.sig` cannot exist even when the write itself fails
    // part-way (ENOSPC, EIO) — `std::fs::write` is not all-or-nothing. Matched by
    // prefix rather than by name, because the pid is the signing process's, not
    // this test's.
    let leftovers: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".incoming"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "staging files were left behind: {leftovers:?}"
    );
}

/// The same key in two slots is refused, and the refusal names slot numbers only.
#[test]
fn the_same_key_in_two_slots_is_refused_without_naming_it() {
    let dir = scratch("dupe");
    let (keys, paths) = key_files(&dir, 1);
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();
    let copy = dir.join("signing-copy.pem");
    std::fs::copy(&paths[0], &copy).unwrap();

    let out = sign_with_files(&[&paths[0], &copy], &binary);
    assert_eq!(
        out.status.code(),
        Some(EXIT_BAD_INPUT),
        "{}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(err.contains("slot 0 and slot 1"), "{err}");
    assert!(!sig_path_for(&binary).exists());
    let pem = std::fs::read_to_string(&paths[0]).unwrap();
    assert_no_key_material(&err, &pem);
    // Nor the public half, which would at least identify the key to a reader of
    // the log; the slot numbers are what an operator acts on.
    assert!(!err.contains(&pubkey_hex(&keys[0])), "{err}");
}

/// More keys than there are slots any reader looks at is a usage error, not a
/// silently-truncated release.
#[test]
fn more_keys_than_slots_is_a_usage_error() {
    let dir = scratch("toomany");
    let (_, paths) = key_files(&dir, 9);
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let out = sign_with_files(&refs, &binary);
    assert_eq!(out.status.code(), Some(EXIT_USAGE), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("usage: veld-sign"));
}

/// `--expect-slot-pubkeys` catches the one release failure nothing else can see:
/// the signing secret holding a **different, valid** key.
///
/// Everything about such a release looks right — the PEM parses, a signature is
/// written, the artifact publishes — and then every privileged install in the
/// field refuses to relaunch onto it. The check must also not echo the expected
/// value back, because a 64-character hex argument cannot be told apart from a
/// hex-encoded private seed.
#[test]
fn an_unexpected_key_in_any_slot_is_refused_and_the_expected_value_is_not_echoed() {
    let dir = scratch("slot0");
    let (keys, paths) = key_files(&dir, 2);
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    // The right key in slot 0 passes.
    let ok = veld_sign()
        .arg("--key-file")
        .arg(&paths[0])
        .args(["--expect-slot-pubkeys", &pubkey_hex(&keys[0])])
        .arg(&binary)
        .output()
        .expect("run veld-sign");
    assert!(ok.status.success(), "{}", stderr_of(&ok));

    // The keys swapped round — the shape of a re-uploaded secret — fails, names
    // the public key it actually found, and never repeats the argv value.
    // The happy path above wrote a `.sig`; clear it first, so the assertion below
    // is about this run rather than about that one.
    std::fs::remove_file(sig_path_for(&binary)).unwrap();
    let sentinel = pubkey_hex(&keys[0]);
    let bad = veld_sign()
        .arg("--key-file")
        .arg(&paths[1])
        .args(["--expect-slot-pubkeys", &sentinel])
        .arg(&binary)
        .output()
        .expect("run veld-sign");
    assert_eq!(
        bad.status.code(),
        Some(EXIT_BAD_INPUT),
        "{}",
        stderr_of(&bad)
    );
    // No `.sig` is left behind. The check runs before the write, so this failure
    // costs nothing on disk — the same contract every other failure here has, and
    // the one this test used to be the single exception to.
    assert!(
        !sig_path_for(&binary).exists(),
        "a slot-0 mismatch left a wrong-key signature beside the binary"
    );
    let err = stderr_of(&bad);
    assert!(err.contains(&pubkey_hex(&keys[1])), "{err}");
    assert!(
        !err.contains(&sentinel),
        "the --expect-slot-pubkeys argument must never be echoed: a 64-character \
         hex argv value is indistinguishable from a private seed.\n{err}"
    );

    // **The case that matters most, and the one a slot-0-only check missed.** A
    // wrong key in a LATER slot ships invisibly — every already-shipped helper
    // still verifies the release through slot 0 — and then wedges every privileged
    // install one release later, when that key becomes the only one a helper
    // accepts. Fleet-wide, sudo-only.
    let (stranger_keys, stranger_paths) = key_files(&dir, 3);
    let _ = std::fs::remove_file(sig_path_for(&binary));
    let expected = format!(
        "{},{}",
        pubkey_hex(&stranger_keys[0]),
        // The second slot is expected to hold key 1 and will be given key 2.
        pubkey_hex(&stranger_keys[1])
    );
    let out = veld_sign()
        .arg("--key-file")
        .arg(&stranger_paths[0])
        .arg("--key-file")
        .arg(&stranger_paths[2])
        .args(["--expect-slot-pubkeys", &expected])
        .arg(&binary)
        .output()
        .expect("run veld-sign");
    assert_eq!(
        out.status.code(),
        Some(EXIT_BAD_INPUT),
        "a wrong key in slot 1 must be refused: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(err.contains("slot 1 was signed by public key"), "{err}");
    assert!(
        err.contains("INVISIBLY"),
        "the hint for a later slot must say why this is worse than slot 0, not \
         repeat slot 0's reasoning: {err}"
    );
    assert!(
        !sig_path_for(&binary).exists(),
        "a slot mismatch left a wrong-key signature beside the binary"
    );
    assert!(!err.contains(&pubkey_hex(&stranger_keys[1])), "{err}");

    // A count mismatch is its own refusal, not a silent pass on the shorter list.
    let short = pubkey_hex(&stranger_keys[0]);
    let out = veld_sign()
        .arg("--key-file")
        .arg(&stranger_paths[0])
        .arg("--key-file")
        .arg(&stranger_paths[1])
        .args(["--expect-slot-pubkeys", &short])
        .arg(&binary)
        .output()
        .expect("run veld-sign");
    // `EXIT_USAGE`, not `EXIT_BAD_INPUT`: the arity is decidable from argv alone,
    // so it is a bad invocation and must not cause a private key to be read.
    assert_eq!(out.status.code(), Some(EXIT_USAGE), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("names 1 key(s) but 2 were given"),
        "{}",
        stderr_of(&out)
    );
    assert!(
        stderr_of(&out).contains("usage: veld-sign"),
        "{}",
        stderr_of(&out)
    );
    assert!(!sig_path_for(&binary).exists());
}

/// A malformed `--expect-slot-pubkeys` is reported as a malformed **flag**, not
/// as a wrong key.
///
/// Measured before this guard existed: `banana` and an empty value both printed
/// "slot 0 was signed by public key … which is not the one this release must
/// carry" — blaming the secret for a mistake in the command line. It failed safe
/// either way, but a release failure is read under pressure and pointing at the
/// wrong file costs real time. The value still must not be echoed.
#[test]
fn a_malformed_expected_pubkey_blames_the_flag_not_the_secret() {
    let dir = scratch("slot0shape");
    let (_, paths) = key_files(&dir, 1);
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    for bad in [
        "banana",
        "",
        "abc",
        &"f".repeat(63),
        &"f".repeat(65),
        &"g".repeat(64),
    ] {
        let out = veld_sign()
            .arg("--key-file")
            .arg(&paths[0])
            .args(["--expect-slot-pubkeys", bad])
            .arg(&binary)
            .output()
            .expect("run veld-sign");
        // `EXIT_USAGE`, not `EXIT_BAD_INPUT`: the module's rule is that a usage
        // error is "the command line itself is wrong, before any input is
        // touched", and this check runs before any private key is read.
        assert_eq!(
            out.status.code(),
            Some(EXIT_USAGE),
            "{bad:?}: {}",
            stderr_of(&out)
        );
        let err = stderr_of(&out);
        assert!(
            err.contains("64 hexadecimal characters"),
            "{bad:?} must be diagnosed as a bad flag, not a wrong key: {err}"
        );
        assert!(
            err.contains("usage: veld-sign"),
            "{bad:?} is a usage error and must print the usage line: {err}"
        );
        assert!(
            !err.contains("which is not the one this release must carry"),
            "{bad:?} was blamed on the key: {err}"
        );
        if !bad.is_empty() {
            assert!(!err.contains(bad), "{bad:?} was echoed back: {err}");
        }
    }
}

/// No key material from **any** slot reaches stderr, on the happy path or on a
/// failure in a later slot.
///
/// #339 found five ways a single key leaked into a message. A second key doubles
/// the surface and adds a shape the single-key tool did not have: an error about
/// key 2 rendered while key 1 is also in memory and in `argv`.
#[test]
fn no_key_material_from_any_slot_reaches_stderr() {
    let dir = scratch("leak");
    let (keys, paths) = key_files(&dir, 2);
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();
    let pems: Vec<String> = paths
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();

    // Happy path: the success report names sources and public keys only.
    let ok = sign_with_files(&[&paths[0], &paths[1]], &binary);
    assert!(ok.status.success(), "{}", stderr_of(&ok));
    for pem in &pems {
        assert_no_key_material(&stderr_of(&ok), pem);
    }

    // A failure in the *second* slot, with the first key still in play.
    let public_pem = keys[1]
        .verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("encode SPKI PEM");
    let wrong = dir.join("wrong.pem");
    std::fs::write(&wrong, &public_pem).unwrap();
    let bad = sign_with_files(&[&paths[0], &wrong], &binary);
    assert_eq!(
        bad.status.code(),
        Some(EXIT_BAD_INPUT),
        "{}",
        stderr_of(&bad)
    );
    for pem in &pems {
        assert_no_key_material(&stderr_of(&bad), pem);
    }

    // And the key passed as a *value* where a path belongs, in every slot
    // position — the accident this tool's whole error discipline exists for.
    for position in 0..paths.len() {
        let mut cmd = veld_sign();
        for (i, path) in paths.iter().enumerate() {
            cmd.arg("--key-file");
            if i == position {
                cmd.arg(&pems[0]);
            } else {
                cmd.arg(path);
            }
        }
        let out = cmd.arg(&binary).output().expect("run veld-sign");
        assert!(!out.status.success());
        assert_no_key_material(&stderr_of(&out), &pems[0]);
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
        // `--expect-slot-pubkeys` twice: the same half-edited shape, and letting
        // the last win would silently ignore the first list.
        vec![
            "--key-file".to_string(),
            key_path.display().to_string(),
            "--expect-slot-pubkeys".to_string(),
            "a".repeat(64),
            "--expect-slot-pubkeys".to_string(),
            "b".repeat(64),
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

/// The same idempotency, for the artifact shape a **rotation** release has.
///
/// `install.sh` re-signs the downloaded helper on macOS, and the whole ordering
/// in `release.yml` exists so that re-sign is byte-identical and the detached
/// signature still matches. That reasoning is about the binary, so it ought to
/// hold for any number of slots — but "ought to" is what the single-slot version
/// of this test was for too, and the release that would discover otherwise is the
/// rotation itself: the one release nobody gets to retry calmly.
///
/// Every slot must still verify afterwards, not just slot 0. A pass that only
/// checked the first would be green on exactly the artifact where the later
/// slots — the *new* key's — are the ones that matter.
#[cfg(target_os = "macos")]
#[test]
fn adhoc_resign_is_byte_idempotent_for_a_multi_slot_signature() {
    let dir = scratch("resign-slots");
    let (keys, paths) = key_files(&dir, 3);

    // Named `veld-helper` for the whole sequence: the ad-hoc identifier is
    // derived from the basename, so both signings must see one path.
    let binary = dir.join("veld-helper");
    std::fs::copy(env!("CARGO_BIN_EXE_veld-sign"), &binary).expect("copy a real Mach-O");

    adhoc_sign(&binary);
    let signed_bytes = std::fs::read(&binary).unwrap();

    let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let out = sign_with_files(&refs, &binary);
    assert!(out.status.success(), "signing failed: {}", stderr_of(&out));

    let sig = std::fs::read(sig_path_for(&binary)).unwrap();
    assert_eq!(sig.len(), 3 * SIG_SLOT_LEN);

    // install.sh's own `codesign --force --sign -`.
    adhoc_sign(&binary);
    let resigned_bytes = std::fs::read(&binary).unwrap();
    assert!(
        signed_bytes == resigned_bytes,
        "ad-hoc re-sign was not byte-idempotent, so install.sh invalidates the \
         multi-slot .sig that CI shipped"
    );

    for (i, key) in keys.iter().enumerate() {
        assert!(
            verify_data_slots(&[key.verifying_key().to_bytes()], &resigned_bytes, &sig),
            "slot {i} stopped verifying after install.sh's re-sign — a helper \
             generation whose only key is this one would refuse the release"
        );
    }
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

/// `--slots` is exactly `--key-env` per entry plus `--expect-slot-pubkeys`.
///
/// This is the flag `release.yml` uses, and it uses it because the whole argument
/// is **derived** from `veld_core::signing::ORG_KEYS` at build time rather than
/// hand-edited — which is what removed all three of the workflow edits a rotation
/// used to need. The equivalence is what makes that safe to rely on: every guard
/// in this file is written against the older spelling, and they only cover the
/// release path if the two spellings really do produce the same thing.
#[test]
fn slots_is_the_same_signature_as_key_env_plus_expected_pubkeys() {
    let dir = scratch("slots-flag");
    let binary = dir.join("veld-helper");
    let payload = b"macho/elf payload signed two ways";
    std::fs::write(&binary, payload).unwrap();

    let keys: Vec<SigningKey> = (0..3).map(|_| throwaway_key()).collect();
    let vars: Vec<String> = (0..3).map(|i| format!("SIGNING_PRIVATE_KEY_{i}")).collect();
    let spec = keys
        .iter()
        .zip(&vars)
        .map(|(k, v)| format!("{}={v}", pubkey_hex(k)))
        .collect::<Vec<_>>()
        .join(",");

    let mut cmd = veld_sign();
    cmd.args(["--slots", &spec]).arg(&binary);
    for (k, v) in keys.iter().zip(&vars) {
        cmd.env(v, k.to_pkcs8_pem(LineEnding::LF).unwrap().as_str());
    }
    let out = cmd.output().expect("run veld-sign");
    assert!(out.status.success(), "signing failed: {}", stderr_of(&out));

    let via_slots = std::fs::read(sig_path_for(&binary)).expect("signature written");
    assert_eq!(via_slots.len(), 3 * SIG_SLOT_LEN);

    // The same three keys through the older spelling must produce the same bytes.
    let mut cmd = veld_sign();
    for v in &vars {
        cmd.args(["--key-env", v]);
    }
    cmd.args([
        "--expect-slot-pubkeys",
        &keys.iter().map(pubkey_hex).collect::<Vec<_>>().join(","),
    ])
    .arg(&binary);
    for (k, v) in keys.iter().zip(&vars) {
        cmd.env(v, k.to_pkcs8_pem(LineEnding::LF).unwrap().as_str());
    }
    let out = cmd.output().expect("run veld-sign");
    assert!(out.status.success(), "signing failed: {}", stderr_of(&out));
    let via_flags = std::fs::read(sig_path_for(&binary)).expect("signature written");
    assert_eq!(
        via_slots, via_flags,
        "--slots must produce byte-identical output to the flags it stands for"
    );

    // And slot order is entry order, which is the compatibility contract.
    for (i, key) in keys.iter().enumerate() {
        let slot = &via_slots[i * SIG_SLOT_LEN..(i + 1) * SIG_SLOT_LEN];
        assert!(
            verify_data(&key.verifying_key().to_bytes(), payload, slot),
            "slot {i} is not signed by --slots entry {i}"
        );
    }
}

/// A `--slots` entry whose secret holds a different key fails, naming neither.
///
/// The wrong-secret guard, reached through the release path. It is the check
/// nothing else in the pipeline can make: a signature by a valid-but-unintended key
/// is indistinguishable from a correct one without knowing which key was meant.
#[test]
fn a_slots_entry_whose_secret_holds_another_key_is_refused() {
    let dir = scratch("slots-wrong-key");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    let meant = throwaway_key();
    let actual = throwaway_key();
    let out = veld_sign()
        .args([
            "--slots",
            &format!("{}=SIGNING_PRIVATE_KEY", pubkey_hex(&meant)),
        ])
        .arg(&binary)
        .env(
            "SIGNING_PRIVATE_KEY",
            actual.to_pkcs8_pem(LineEnding::LF).unwrap().as_str(),
        )
        .output()
        .expect("run veld-sign");

    assert!(!out.status.success(), "a wrong key must fail the release");
    assert!(
        !sig_path_for(&binary).exists(),
        "no .sig may be written when any slot's key is wrong"
    );
    let err = stderr_of(&out);
    // The key the secret turned out to hold IS named — a public key, and the only
    // way an operator can tell which key a secret holds without reading it. The
    // **expected** value is not, because 64 hex characters cannot be told apart
    // from a private seed, and neither is any part of the PEM.
    assert!(
        err.contains(&pubkey_hex(&actual)),
        "the found public key is what tells the operator which key the secret \
         holds: {err}"
    );
    assert!(
        !err.contains(&pubkey_hex(&meant)),
        "the expected value must never be echoed: {err}"
    );
    for line in actual
        .to_pkcs8_pem(LineEnding::LF)
        .unwrap()
        .lines()
        .filter(|l| !l.starts_with("-----"))
    {
        assert!(!err.contains(line), "veld-sign echoed key material: {err}");
    }
}

/// A malformed `--slots` argument is a usage error that echoes neither half.
///
/// The left half of an entry is 64 characters this tool cannot tell apart from a
/// hex-encoded private seed, and an entry that failed to split is one whose halves
/// are not identified at all — so nothing from it is quoted back.
#[test]
fn a_malformed_slots_entry_echoes_nothing_from_it() {
    let dir = scratch("slots-malformed");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();

    let key = throwaway_key();
    let pem = key.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
    // No `=`: the whole entry is unidentified, and here it happens to be a PEM.
    let out = veld_sign()
        .args(["--slots", pem.trim()])
        .arg(&binary)
        .output()
        .expect("run veld-sign");

    assert_eq!(
        out.status.code(),
        Some(2),
        "a bad command line is EXIT_USAGE"
    );
    let err = stderr_of(&out);
    for line in pem.lines().filter(|l| !l.starts_with("-----")) {
        assert!(
            !err.contains(line),
            "veld-sign echoed a --slots entry: {err}"
        );
    }
    assert!(
        !sig_path_for(&binary).exists(),
        "a usage error must not reach the signing path at all"
    );
}

/// `--slots` and `--expect-slot-pubkeys` together is a usage error, in both orders.
///
/// `--slots` already carries the expected key for every slot. A step passing both
/// is a half-finished edit, and letting one silently win is how a slot ends up
/// checked against the wrong key — the mistake the expected list exists to catch.
#[test]
fn slots_and_expect_slot_pubkeys_together_is_refused() {
    let dir = scratch("slots-both");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();
    let hex = pubkey_hex(&throwaway_key());

    for args in [
        vec![
            "--slots".to_owned(),
            format!("{hex}=SIGNING_PRIVATE_KEY"),
            "--expect-slot-pubkeys".to_owned(),
            hex.clone(),
        ],
        vec![
            "--expect-slot-pubkeys".to_owned(),
            hex.clone(),
            "--slots".to_owned(),
            format!("{hex}=SIGNING_PRIVATE_KEY"),
        ],
    ] {
        let out = veld_sign()
            .args(&args)
            .arg(&binary)
            .output()
            .expect("run veld-sign");
        assert_eq!(
            out.status.code(),
            Some(2),
            "both flags together must be EXIT_USAGE: {}",
            stderr_of(&out)
        );
    }
}

/// `--slots` plus `--key-env` is a usage error that says so.
///
/// `--slots` carries every slot's key and secret, so a `--key-env` beside it is a
/// half-finished edit. Without its own message it fell through to the count check
/// and reported `--expect-slot-pubkeys names 1 key(s) but 2 were given` — naming a
/// flag the caller never passed, on a release path where the first thing anyone
/// suspects is the signing secret.
#[test]
fn slots_mixed_with_a_key_flag_names_the_right_flag() {
    let dir = scratch("slots-mixed");
    let binary = dir.join("veld-helper");
    std::fs::write(&binary, b"payload").unwrap();
    let hex = pubkey_hex(&throwaway_key());

    for extra in [
        vec!["--key-env".to_owned(), "SIGNING_PRIVATE_KEY_2".to_owned()],
        vec!["--key-file".to_owned(), "/etc/veld/signing.pem".to_owned()],
    ] {
        let out = veld_sign()
            .args(["--slots", &format!("{hex}=SIGNING_PRIVATE_KEY")])
            .args(&extra)
            .arg(&binary)
            .output()
            .expect("run veld-sign");
        let err = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "must be EXIT_USAGE: {err}");
        assert!(
            err.contains("--slots carries every slot's key and secret"),
            "the message must name --slots, not the flag the caller never passed: {err}"
        );
    }
}
