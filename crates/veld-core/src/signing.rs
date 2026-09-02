//! Org signature verification for the privileged `veld-helper`.
//!
//! The privileged helper runs as root and relaunches itself (binary watcher,
//! `restart`, `shutdown`) so the service manager can pick up a new version. It
//! used to do that from a **user-writable** directory — the #247 escalation: any
//! process with the installing user's privileges could swap the binary and get
//! root on the next relaunch. The fix: the org signs each release helper with its
//! ed25519 private key into a detached `<binary>.sig`, and the *running* helper
//! verifies the on-disk binary against its embedded public key before it will
//! `exit(0)` onto a replacement.
//!
//! Since #262 the binary lives in a **root-owned** store
//! ([`crate::paths::privileged_helper_dir`]) and the only way in is an install RPC
//! that verifies a signature and refuses to go backwards, so this gate is no longer
//! the only thing standing between the installing user and root — but it is still
//! the thing that decides what root executes, and an install that has not migrated
//! yet is still in the original shape. Written down because the previous version of
//! this paragraph described a layout two releases old, which is the first thing a
//! reader of this module sees.
//!
//! The crypto and the org key live **here** in `veld-core`, not in `veld-helper`,
//! so `veld doctor` can check a helper's signature with the exact same
//! primitive and key — a duplicated constant would drift when the key rotates,
//! and two different verify paths would disagree about the one thing a security
//! gate must not guess.
//!
//! Verification is **fail-closed**: any failure — missing/oversized `.sig`,
//! unreadable binary, mismatched signature — returns false, and the callers
//! (the helper's relaunch paths, and the doctor row) treat that as "not safe to
//! relaunch onto", never "assume safe".
//!
//! # Key rotation (#261 slice C)
//!
//! A leaked private key needs a way out that does not itself depend on the
//! leaked key, and the only trust an already-installed helper has is the key
//! compiled into it. So rotation works by **accumulating** trust rather than
//! replacing it:
//!
//!   * `<binary>.sig` is a list of 64-byte signatures over the binary — one
//!     **slot** per key the release was signed by. Slot 0 belongs to
//!     [`ORG_KEY_1`] and cannot move, because every helper shipped up to
//!     v16.59.0 reads exactly bytes `0..64` and checks them against that one
//!     key. With a single key the file is exactly 64 bytes, i.e. byte-identical
//!     to what shipped before this existed.
//!   * A helper accepts a binary when **any** slot verifies under **any** key in
//!     [`ORG_SIGNING_KEYRING`] — the list compiled into that helper.
//!   * Rotation is therefore a release: one whose keyring gained a key and whose
//!     `.sig` gained a slot. Retirement is a later release whose keyring lost a
//!     key while the `.sig` keeps its slot for helpers that still need it.
//!
//! **Trust rides the binary and nothing is persisted**, which is the property
//! everything else here rests on:
//!
//!   * *Rotation cannot be replayed.* Changing which keys a helper trusts means
//!     changing the binary it executes, and [`crate::helper_store`] already
//!     refuses to install anything older than the newer of the running and the
//!     installed version. Trust monotonicity is inherited from version
//!     monotonicity rather than being a second thing to get right.
//!   * *Adopting a key **is** executing the binary that carries it.* There is no
//!     state in which an attacker has captured a machine's trust root without
//!     already running code on it as root — which is why no separate
//!     successor-commitment is needed here, and why a keyring persisted to disk
//!     (the shape that does need one) was rejected. See
//!     `docs/signing-key-rotation.md`.
//!
//! What this honestly does **not** buy, stated rather than implied: rotation
//! makes a leaked key *insufficient*, never *useless*. Slot 0's signature has to
//! keep being produced for as long as any pre-rotation helper might still be in
//! the field, and nothing rescues a machine an attacker reached before the
//! rotating release landed on it.

use std::io::Read;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, VerifyingKey};

/// One raw ed25519 public key.
pub type PubKey = [u8; 32];

/// The org's first ed25519 public key — the **only** key every helper shipped up
/// to and including v16.59.0 has compiled in, and therefore the key whose
/// signature must occupy slot 0 of `<binary>.sig`.
///
/// Derived from `notes/veld-signing-ed25519.pub` (SPKI); the private key lives in
/// the org vault and the GitHub `SIGNING_PRIVATE_KEY` secret.
///
/// **Its slot position is not a convention, it is a compatibility contract.**
/// Those helpers read exactly bytes `0..64` and verify them against this key, and
/// their code cannot be changed. Move this key out of slot 0 and every one of
/// them refuses to relaunch onto the next release — which leaves sudo as the only
/// repair channel, the single outcome #338's rules forbid.
pub const ORG_KEY_1: PubKey = [
    0x9d, 0x75, 0xa4, 0xc5, 0x5c, 0x02, 0xb4, 0x6e, 0x53, 0xa3, 0x0d, 0x1d, 0xf8, 0x84, 0xc8, 0xaa,
    0xf0, 0xd3, 0x90, 0x23, 0x06, 0xd1, 0xc6, 0xee, 0x53, 0x60, 0x32, 0x99, 0xa3, 0x1b, 0x31, 0x56,
];

/// One org signing key, and everything about it that a release has to know.
///
/// **This table is the whole trust root, and rotating is appending to it.** Before
/// it there were two hand-kept lists plus a hand-kept count, and a rotation edited
/// all three plus three lines of `release.yml`; the count and the workflow's
/// expected-key list were transcriptions of what the lists already said. They are
/// derived now, so the transcription cannot rot and there is nothing left to
/// forget.
///
/// It is a **table of records rather than two sets** for a second reason, and it is
/// the load-bearing one. Two flat lists can only express *state*: removing a key
/// from a keyring destroys, from the post-image, every trace that the key ever
/// existed — so no static check can tell a retirement apart from a steady state
/// that never held that key, and the two-release rule was therefore unguardable in
/// principle. A row that **survives** its own retirement is the only place that
/// evidence can live. See [`KeyStatus::Retired`].
#[derive(Debug, Clone, Copy)]
pub struct OrgKey {
    /// The public half. This key's **slot index** in `<binary>.sig` is its index
    /// in [`ORG_KEYS`], which is why rows are appended and never reordered.
    pub key: PubKey,

    /// The GitHub Actions secret holding the private half, which signs this key's
    /// slot at release time.
    ///
    /// Recorded *here*, beside the key, rather than left implicit in the order of
    /// flags in `release.yml`. That pairing is the thing this format has no room
    /// for — slot position is its only key identifier — so a mis-pairing between a
    /// key and the secret that signs its slot is invisible in the artifact and
    /// fatal one release later. `ci.yml` checks every name here against the
    /// workflow's `env:` block on every pull request.
    ///
    /// **Names are positional and immortal**, never relative to *now*.
    /// `SIGNING_PRIVATE_KEY` holds the original key forever and is never
    /// re-uploaded; a new key takes the next free number. A recency-based name — a
    /// `_OLD`/`_CURRENT` pair — reads as the obvious scheme and is fatal on the
    /// **second** rotation: slot 0 would then hold the second key, which no helper
    /// shipped up to v16.59.0 has ever trusted, and the first key would have no
    /// slot at all.
    pub secret: &'static str,

    /// `Cargo.toml`'s `version` at the moment this row was written — i.e. the
    /// last release before the one that introduces this key. Copy it verbatim.
    ///
    /// **You can always know this, and that is the whole point.** `Cargo.toml`
    /// holds the *previous* release until semantic-release bumps it after the
    /// merge, so a pull request can never name the version it will itself ship as
    /// — but it can always name the one it was written against. Together with
    /// [`KeyStatus::Retired`]'s matching field, that is what makes the two-release
    /// rule checkable from one file with no git history: see
    /// `one_release_never_both_adds_a_key_and_retires_one`.
    ///
    /// [`ORG_KEY_1`] carries `0.0.0` because it predates this table and every
    /// release in it.
    pub added_after: &'static str,

    /// Whether this build still accepts a signature by this key.
    pub status: KeyStatus,
}

/// Whether a build accepts a key — and, once it does not, the evidence that
/// retiring it was safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    /// This build accepts a signature by this key.
    Accepted,

    /// This build no longer accepts it. Releases keep carrying its slot, because
    /// some helper generation in the field accepts *only* this key.
    ///
    /// **This variant is the two-release rule, mechanised**, and that is why it
    /// carries a mandatory field instead of being a `bool`. A one-character flip is
    /// the cheapest dangerous edit in the repository and invisible in a skim;
    /// constructing a variant with a value in it is an act of authorship.
    Retired {
        /// `Cargo.toml`'s `version` at the moment this key was retired — the same
        /// thing [`OrgKey::added_after`] records, for the other half of the edit.
        /// Copy it verbatim.
        ///
        /// **The rule these two fields express:** no single release may both add a
        /// key and retire one. A pull request that tried would have to write the
        /// *same* version in both fields — the one `Cargo.toml` shows right now —
        /// and `one_release_never_both_adds_a_key_and_retires_one` fails exactly
        /// that. Neither needs git history; both are simply what the tree says.
        ///
        /// Not "the next version is unknowable": `release.yml`'s `plan` job runs
        /// semantic-release in dry-run on every non-draft pull request and prints
        /// it. The point is narrower and survives that — the tree cannot be made to
        /// *say* the pull request has already shipped, and
        /// `every_key_lifecycle_date_names_a_release_that_has_happened` refuses a
        /// value newer than `Cargo.toml`, so writing the upcoming version is caught
        /// too.
        ///
        /// **The hole this leaves, named rather than glossed:** if the branch's
        /// `Cargo.toml` moves between the two edits — an unrelated release lands,
        /// the branch is updated, then the retirement is written — the two dates
        /// are both *honest* and *different*, neither equals the current version,
        /// and the test sees nothing. The tree cannot tell "added last release"
        /// from "added in this pull request", and nothing written in this file
        /// can. `ci.yml`'s *One release never both adds a key and retires one* is
        /// the layer that closes it, by reading the pull request's own diff; this
        /// one is what still works on a direct push to `main`, where there is no
        /// diff to read.
        ///
        /// What remains uncaught by both is a value *invented* for
        /// `added_after` — neither the truth nor anything already written down.
        /// `added_after_increases_down_the_table` removes the stale values that are
        /// lying around to copy (row 0's `0.0.0`, any earlier row's version), so
        /// what is left takes deliberate arithmetic. Nothing in the tree can prove
        /// a date honest.
        retired_after: &'static str,
    },
}

/// Every org key, in slot order. **Append only; never reorder, never delete.**
///
/// Row 0 is [`ORG_KEY_1`] forever — see that constant for why its slot position is
/// a compatibility contract rather than a convention.
///
/// * **Rotating** = append one row, `status: KeyStatus::Accepted`.
/// * **Retiring** = change one row's status to [`KeyStatus::Retired`], in a
///   **later** release. The row stays; its slot keeps being produced.
///
/// Deleting a row is the one edit nothing here can see, because it destroys the
/// same evidence a retirement preserves. It also drops a slot, which strands every
/// helper generation that accepts only that key — the announced flag day described
/// in `docs/signing-key-rotation.md`, not something a rotation does.
pub const ORG_KEYS: &[OrgKey] = &[OrgKey {
    key: ORG_KEY_1,
    secret: "SIGNING_PRIVATE_KEY",
    // Predates this table and every release in it.
    added_after: "0.0.0",
    status: KeyStatus::Accepted,
}];

/// A release may not claim more slots than any reader looks at.
const _: () = assert!(ORG_KEYS.len() <= MAX_SIG_SLOTS);

/// **A build that trusts nothing must not compile.**
///
/// Flipping the last `Accepted` row to `Retired` is one field, and the result is a
/// helper that refuses every relaunch, refuses `restart` and `shutdown`, refuses
/// every candidate, and leaves sudo as the only repair —
/// `the_keyring_is_never_empty` catches it, but only for somebody who runs the
/// tests. An engineer who builds and installs locally bricks their own machine
/// first and reads the test failure second.
const _: () = assert!(accepted_key_count(ORG_KEYS) > 0);

/// The keys **this build accepts** a signature from — [`ORG_KEYS`]' accepted rows.
///
/// A binary verifies when any slot of its `.sig` verifies under any key here, so
/// this list is what "trusted" means for the helper that links it. It rides the
/// binary deliberately — see the module doc — so a helper's trust changes only
/// when the binary it executes changes, and that transition is already governed
/// by [`crate::helper_store`]'s version floor.
///
/// Derived rather than hand-kept: expressing an addition and a retirement as edits
/// to *one* row's status is what makes the combination of the two checkable at all.
/// What that combination costs is stated exactly — and asserted — by
/// `a_combined_release_is_a_truncation_window_not_a_stranding`: not a refusal, but
/// the truncation window on pre-v16.60.0 installers. See
/// `docs/signing-key-rotation.md`'s "Why retirement is a separate release".
pub const ORG_SIGNING_KEYRING: &[PubKey] =
    &accepted_keys::<{ accepted_key_count(ORG_KEYS) }>(ORG_KEYS);

/// The keys every release must still be **signed by**, whether or not this build
/// would accept them — i.e. every row of [`ORG_KEYS`], in slot order.
///
/// A key stays here after it stops being accepted, because some helper generation
/// in the field accepts *only* that key and a release carrying no slot for it is a
/// release that generation refuses. This being *every* row rather than a second
/// hand-kept list is what makes "grows, never shrinks" a derivation instead of a
/// convention: forgetting to keep a retired key's slot is now unrepresentable.
pub const ORG_REQUIRED_SLOT_KEYS: &[PubKey] = &all_keys::<{ ORG_KEYS.len() }>(ORG_KEYS);

/// How many 64-byte slots `<binary>.sig` must carry for a release to be
/// installable by every helper generation still in the field: one per row of
/// [`ORG_KEYS`].
///
/// It used to be a hand-written number, because it is read from **two** places
/// that cannot evaluate Rust — `release.yml`'s signing step and `ci.yml`'s gate on
/// it. Both now read [`ORG_KEYS`] itself, by the same regex, so the number has one
/// home again.
pub const RELEASE_SIG_SLOTS: usize = ORG_KEYS.len();

/// How many rows of `keys` are [`KeyStatus::Accepted`].
///
/// Takes the table rather than reading [`ORG_KEYS`] so that
/// `the_derivation_keeps_a_retired_rows_slot` can hand it a table that HAS a
/// retired row. Today's has none, which made "a retired row keeps its slot" — the
/// property the two-release rule's whole cost analysis rests on — unfalsifiable:
/// with every row accepted, the keyring and the required list coincide, so no test
/// could tell a correct derivation from one that dropped retired rows. Found by
/// mutating exactly that and watching all 47 tests stay green.
///
/// A `const fn` rather than a `Vec` at runtime so [`ORG_SIGNING_KEYRING`] stays a
/// `&'static [PubKey]` — every caller, including the root helper's relaunch gate,
/// keeps taking a plain slice with no allocation on a path that runs on a timer.
const fn accepted_key_count(keys: &[OrgKey]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < keys.len() {
        if matches!(keys[i].status, KeyStatus::Accepted) {
            n += 1;
        }
        i += 1;
    }
    n
}

/// The accepted rows' keys, in slot order. `N` must be [`accepted_key_count`].
const fn accepted_keys<const N: usize>(keys: &[OrgKey]) -> [PubKey; N] {
    let mut out = [[0u8; 32]; N];
    let mut i = 0;
    let mut n = 0;
    while i < keys.len() {
        if matches!(keys[i].status, KeyStatus::Accepted) {
            out[n] = keys[i].key;
            n += 1;
        }
        i += 1;
    }
    out
}

/// Every row's key, in slot order. `N` must be `ORG_KEYS.len()`.
const fn all_keys<const N: usize>(keys: &[OrgKey]) -> [PubKey; N] {
    let mut out = [[0u8; 32]; N];
    let mut i = 0;
    while i < N {
        out[i] = keys[i].key;
        i += 1;
    }
    out
}

/// One slot of a detached signature file: a raw ed25519 signature.
pub const SIG_SLOT_LEN: usize = 64;

/// Most slots [`read_detached_sig_slots`] will read.
///
/// A bound rather than a limit on what may be *written*: the file sits beside a
/// binary in a directory the installing user can write, and on the install RPC it
/// is a path they *name*, so an unbounded read is a memory DoS on every relaunch
/// attempt, every doctor run and every install request. Eight is far more keys
/// than the org will ever have live at once, and anything past it is ignored the
/// same way the pre-rotation reader ignored everything past byte 64.
///
/// **It bounds root-side work as well as memory, and that is the newer reason.**
/// [`verify_data_slots`] does up to `slots × keyring.len()` full ed25519
/// verifications, each hashing the whole binary — ed25519 hashes `R ‖ A ‖ M`, so
/// nothing can be shared between slots or keys — and the caller who writes the
/// `.sig` chooses that multiplier. `any()` short-circuits, so the worst case is
/// only reached by input that verifies under *nothing*, i.e. a refusal.
///
/// The real numbers, measured on an M-series Mac in a release build rather than
/// guessed, because the first version of this comment was wrong by about 70x and
/// a wrong number here is what the next person to raise this constant will trust:
///
///   * ~0.24 s per SHA-512 pass over 128 MiB, which is `helper_store`'s
///     `MAX_CANDIDATE_BYTES` — the size an *unprivileged caller* may hand the
///     install RPC. Eight junk slots against one key is therefore ~1.9 s of root
///     CPU per request, against ~0.24 s before this existed. The install RPC does
///     **not** go through [`classify_binary_signature`] — it calls
///     [`verify_data_slots`] on bytes it already holds — so it pays that once.
///   * The **relaunch and doctor** paths do go through
///     [`classify_binary_signature`], which decides `Untrusted` twice before
///     believing it, so on those paths a refusal costs two passes: **double** the
///     figures above. That is the path with no lock in front of it and a
///     ten-second tick behind it, so the doubling is the number that matters. It
///     buys not printing a tampered-root-binary paragraph on a machine where an
///     update has just landed correctly; judged worth it, and stated rather than
///     buried.
///   * Post-retirement the multiplier is `slots × (active + retired)`, because
///     [`classify_binary_signature`] sweeps the active keys and then the retired
///     ones. With one retired key and two active, 8 × 3 = 24 — and 48 on the
///     relaunch/doctor path once the retry below is counted.
///
/// **The install lock does not cover the relaunch path.** The helper's binary
/// watcher calls [`classify_binary_signature`] every `BINARY_WATCH_INTERVAL` for
/// as long as the on-disk file differs from its baseline — so on an install not yet
/// migrated to the root-owned store, where the installing user can write both
/// files, the cost is paid on a timer with nothing serialising it. That read used
/// to be an unbounded `std::fs::read` (as the pre-rotation
/// `verify_binary_signed`'s was), which made the figures above meaningless there
/// because the *size* was the attacker's to choose. It is bounded now — see
/// [`MAX_VERIFIED_BINARY_BYTES`] — so the figures apply, and the doubling below is
/// a doubling of something finite.
///
/// Byte-identical slots are collapsed before verifying, which removes the lazy
/// version of that multiplier for free. It does not remove the deliberate one —
/// eight *distinct* junk slots still cost eight passes — and closing that
/// properly needs a key identifier per slot, which the format deliberately does
/// not have (see `docs/signing-key-rotation.md`). Stated as a residual rather
/// than implied: it is a CPU cost to an attacker who is already the installing
/// user, on a machine where the same user can already keep the daemon busy.
pub const MAX_SIG_SLOTS: usize = 8;

/// Whether `data` carries a valid ed25519 signature by `pubkey` in `sig`.
///
/// **One key, one 64-byte signature** — the primitive, and deliberately still
/// strict about the length. This is the exact shape the pre-rotation helpers
/// verify, so keeping it unchanged (and tested) is what lets the slot reader
/// above it be added without wondering whether the floor moved.
pub fn verify_data(pubkey: &PubKey, data: &[u8], sig: &[u8]) -> bool {
    if sig.len() != SIG_SLOT_LEN {
        return false;
    }
    let Ok(vk) = VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(sig) else {
        return false;
    };
    vk.verify_strict(data, &sig).is_ok()
}

/// Whether any 64-byte slot of `sig` verifies `data` under any key in `keyring`.
///
/// A slot that verifies under nothing is simply skipped, which is what makes the
/// format additive: a future release may append whatever it likes after the slots
/// this build understands, and a build that does not understand it reads noise
/// that matches no key rather than failing. That tolerance is the same one the
/// pre-rotation reader had by accident, made deliberate.
///
/// A trailing partial slot is ignored for the same reason. `sig` is expected to
/// be bounded by the caller — [`read_detached_sig_slots`] is what does that.
pub fn verify_data_slots(keyring: &[PubKey], data: &[u8], sig: &[u8]) -> bool {
    verifying_key(keyring, data, sig).is_some()
}

/// [`verify_data_slots`], but naming the key that verified.
///
/// The verdict on its own answers "may root execute this", which is all the gates
/// need. **Which** key answered it is what a person needs when a machine is
/// somewhere unexpected in a rotation window: a helper's keyring and a release's
/// slots are two lists that move at different times, and "signature OK" is the same
/// sentence whether this machine is on the key before the rotation or the one
/// after. `veld doctor` says which; nothing else does, and nothing on the machine
/// records it.
///
/// A public key, so there is nothing here that must not be printed.
pub fn verifying_key(keyring: &[PubKey], data: &[u8], sig: &[u8]) -> Option<PubKey> {
    let mut found = None;
    any_slot_verifies(sig, |slot| {
        found = keyring
            .iter()
            .find(|key| verify_data(key, data, slot))
            .copied();
        found.is_some()
    });
    found
}

/// Where `key` sits in [`ORG_KEYS`], as a 1-based slot number, and how many rows
/// there are — `(2, 3)` reads as "org key 2 of 3".
///
/// The index is the key's **permanent identity**: rows are appended and never
/// reordered, so key 2 stays key 2 across every later rotation and retirement, and
/// the number in a `veld doctor` row a user pastes into an issue means the same
/// thing a year later. Falls back to `None` for a key that is not in the table,
/// which is only reachable from a test.
pub fn org_key_position(key: &PubKey) -> Option<(usize, usize)> {
    ORG_KEYS
        .iter()
        .position(|k| &k.key == key)
        .map(|i| (i + 1, ORG_KEYS.len()))
}

/// Whether any **distinct** 64-byte slot of `sig` satisfies `verify`.
///
/// Split from [`verify_data_slots`] so the deduplication is *observable*, which is
/// the only way it survives. It is a pure cost optimisation: every functional test
/// passes identically with or without it, so an engineer tidying what looks like
/// unnecessary bookkeeping around a straightforward `any()` would reopen the
/// CPU-amplification [`MAX_SIG_SLOTS`] spends its doc comment bounding, with
/// nothing red anywhere. `each_distinct_slot_is_verified_at_most_once` counts the
/// calls, and counting them needs this seam.
fn any_slot_verifies(sig: &[u8], mut verify: impl FnMut(&[u8]) -> bool) -> bool {
    let mut tried: Vec<&[u8]> = Vec::new();
    for slot in sig.chunks_exact(SIG_SLOT_LEN) {
        // Byte-identical slots are verified once. Each verification hashes the
        // whole binary and the `.sig` is written by the caller, so a repeated slot
        // is free work handed to root — see [`MAX_SIG_SLOTS`] for the measured
        // cost and for what this does *not* fix.
        if tried.contains(&slot) {
            continue;
        }
        tried.push(slot);
        if verify(slot) {
            return true;
        }
    }
    false
}

/// `<path>.sig` — the sibling detached signature (append, not replace).
pub fn sig_path_for(binary: &Path) -> PathBuf {
    let mut s = binary.as_os_str().to_os_string();
    s.push(".sig");
    PathBuf::from(s)
}

/// Largest binary this will read in order to verify it.
///
/// The read used to be an unbounded `std::fs::read`, on a path an unprivileged
/// caller can influence, and each slot verification hashes the whole thing.
///
/// Measured rather than guessed, because the failure mode of a bound set too low is
/// refusing a genuine artifact: the largest thing that legitimately reaches this is
/// a **debug** build of the helper, 31 MB on this machine, and that only on a
/// developer running `veld doctor` against `target/debug`. A release helper is
/// ~6 MB. So this is four times the largest real artifact — deliberately the same
/// figure `helper_store`'s `MAX_CANDIDATE_BYTES` uses, since they bound the same
/// hostile shape for the same reason.
///
/// Two things checked rather than assumed. `release.yml` has no `lipo` step — it
/// builds `aarch64-apple-darwin` and `x86_64-apple-darwin` as separate targets — so
/// there is no universal binary to double the figure; the bound would still hold for
/// one. And every call site here passes a `veld-helper` path, never `veld-daemon` or
/// the ~40 MB `caddy`. A *Linux* debug build is the one artifact that could approach
/// this, because it keeps DWARF inside the binary where macOS leaves it in the `.o`
/// files — and it is never org-signed, so it is refused for want of a signature with
/// or without the bound.
pub const MAX_VERIFIED_BINARY_BYTES: u64 = 128 * 1024 * 1024;

/// Read a **regular file** at `path`, up to `limit` bytes, without ever blocking
/// on a special file. `None` when it is not a readable regular file; otherwise the
/// bytes and whether the file was **longer** than `limit`.
///
/// The two callers want opposite things from that flag and both are deliberate:
/// the `.sig` reader **tolerates** a longer file and keeps the prefix, because a
/// future release may append something this build does not understand and the
/// pre-rotation reader tolerated exactly that; the binary reader **refuses** it,
/// because a helper larger than the bound is not one of ours and fail-closed is the
/// rule everywhere else here. Folding those into one "return None if too long"
/// silently broke the tolerance, which is why the flag is explicit.
///
/// The whole discipline in one place, because this module needed it twice and
/// getting one of the two wrong is exactly what happened:
///
///   * a pre-open `metadata` check, because some device nodes act on `open`
///     itself: `/dev/watchdog` arms the hardware watchdog as root, and dropping the
///     descriptor without the magic close reboots the machine. Nothing after the
///     open can undo that;
///   * `O_NONBLOCK`, because a plain `open(O_RDONLY)` on a **FIFO blocks until
///     somebody opens the write end** — parking the helper's watcher, its
///     `shutdown` handler, `veld doctor`, or a blocking-pool thread of the root
///     daemon, permanently and uncancellably;
///   * an `is_file()` check on the **descriptor**, never on the path, because a
///     path can be swapped between the check and the open and a descriptor cannot;
///   * a bound applied to the *read*, not to a prior `stat` — a stat answers about
///     the file that was there a moment ago, and this path's whole hazard is a file
///     that changes underneath us.
///
/// The pre-open `metadata` is a **TOCTOU and not a free one**: for the watchdog
/// case the side effect *is* the open, so losing that race costs precisely what
/// the check exists to prevent. It narrows the window; it does not close it, and
/// closing it needs `O_NOFOLLOW` or `O_PATH` — `O_NOFOLLOW` being off the table
/// because a path reached through a symlink must keep working. Both halves of that
/// are pinned, one test per caller:
/// `a_sig_symlinked_to_a_non_regular_file_is_refused` and
/// `a_binary_symlinked_to_a_non_regular_file_is_refused`, each of which refuses a
/// symlink to a FIFO *and* accepts a symlink to a real file. Said plainly because
/// an earlier wording here claimed the race was harmless, and because the earlier
/// version of this comment cited only the `.sig` test from the binary's guard.
fn read_regular_file_bounded(path: &Path, limit: u64) -> Option<(Vec<u8>, bool)> {
    use std::os::unix::fs::OpenOptionsExt;

    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    // `take(limit + 1)` + `read_to_end`: one extra byte is what distinguishes "a
    // file exactly at the bound" from "a longer one", and `read_to_end` rather than
    // a single `read` because a single `read` may return fewer bytes than asked for
    // even on a regular file. The bound is on the *read*, not on a prior `stat`.
    // `saturating_add`: `limit + 1` would wrap at `u64::MAX` and `take(0)` then
    // returns a *successful empty read* — the one shape both callers read as
    // "verified nothing". Unreachable with today's two constant call sites, and
    // not worth leaving as a trap for a third.
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    let over = bytes.len() as u64 > limit;
    bytes.truncate(limit as usize);
    Some((bytes, over))
}

/// The detached signature slots beside `binary`, or `None` when there is not
/// even one readable 64-byte slot.
///
/// Bounded to [`MAX_SIG_SLOTS`] slots: the file can sit in a user-writable
/// directory, so a huge one must not be slurped into memory here — that would be
/// a memory DoS on every relaunch attempt, every doctor run, and (since #262)
/// every install request.
///
/// **Returns whole slots only.** A file of 100 bytes yields the first 64 and
/// drops the rest, exactly as the pre-rotation reader did; a file of 63 yields
/// `None`, exactly as `read_exact` into a `[u8; 64]` did. Both behaviours are
/// load-bearing rather than incidental — `the_pre_rotation_reader_still_accepts_a_multi_slot_release`
/// is what holds them.
fn read_detached_sig_slots_inner(binary: &Path) -> Option<Vec<u8>> {
    match read_detached_sig_slots_classified(binary) {
        SigRead::Slots(slots) => Some(slots),
        SigRead::NoWholeSlot | SigRead::Unreadable => None,
    }
}

/// What reading the slots beside a binary produced.
///
/// Three outcomes, because the middle one is **not** a read failure and treating
/// it as one put it on the wrong side of [`classify_binary_signature`]'s retry
/// rule. `install.sh` writes the lib-dir `.sig` with `cp`, which truncates in
/// place — so there is a real window in which the file reads fine and holds no
/// whole slot, and that is a torn write, exactly what the retry exists for.
///
/// The narrowing bought nothing at that boundary either: somebody wanting two
/// reads per tick plants 64 bytes of junk, which is a verification failure and is
/// retried anyway.
enum SigRead {
    /// At least one whole 64-byte slot.
    Slots(Vec<u8>),
    /// Read fine, but 0–63 bytes: no whole slot. A torn write, not a bad path.
    NoWholeSlot,
    /// The path could not be read at all — missing, not a regular file, or an I/O
    /// error. Not retried.
    Unreadable,
}

/// [`read_detached_sig_slots_inner`], keeping the distinction the retry needs.
fn read_detached_sig_slots_classified(binary: &Path) -> SigRead {
    let Some((mut slots, _over)) =
        read_regular_file_bounded(&sig_path_for(binary), (MAX_SIG_SLOTS * SIG_SLOT_LEN) as u64)
    else {
        return SigRead::Unreadable;
    };
    slots.truncate(slots.len() - slots.len() % SIG_SLOT_LEN);
    if slots.is_empty() {
        return SigRead::NoWholeSlot;
    }
    SigRead::Slots(slots)
}

/// [`read_detached_sig_slots_inner`] for callers outside this module.
///
/// The install path (`crate::helper_store`) needs the slots themselves and not
/// just a verdict, because it installs the `.sig` it verified against rather than
/// re-reading the caller's path a second time — and it must install **every**
/// slot, not only the one that happened to verify. A slot dropped on the way into
/// the store is a slot the *next* helper generation cannot find.
pub fn read_detached_sig_slots(binary: &Path) -> Option<Vec<u8>> {
    read_detached_sig_slots_inner(binary)
}

/// How much this build trusts the signature beside a binary.
///
/// Three answers rather than a bool because the middle one is a real state with
/// its own remedy, and reporting it as "not signed by us" would read as tampering
/// on a machine where nothing is wrong. See [`SigTrust::RetiredOnly`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigTrust {
    /// A slot verified under a key in [`ORG_SIGNING_KEYRING`]. Safe.
    Active,
    /// No slot verified under an active key, but one verified under a key that
    /// *used* to be active — so the org really did build these bytes, and this
    /// build has simply stopped accepting that key.
    ///
    /// The way a healthy machine reaches this: a pre-rotation helper's install
    /// RPC kept only the first 64 bytes of the `.sig` it was handed (its
    /// `Candidate` held a `[u8; 64]`), so a machine that jumped straight from a
    /// pre-rotation release to a post-retirement one has a store holding the
    /// right binary beside a signature with only the retired slot left in it.
    /// Nothing is compromised — the bytes are genuine and no *incoming* candidate
    /// is accepted on this basis — and re-running the installer
    /// ([`INSTALLER_COMMAND`]) writes the full slot list and clears it.
    /// **Not `veld update`**, which is a no-op on a machine already on the latest
    /// release, and a machine in this state is on the latest release by
    /// construction; see [`INSTALLER_COMMAND`] for why. Fail-closed still applies:
    /// this is not accepted anywhere, it is only *diagnosed* differently.
    RetiredOnly,
    /// Nothing verified. Missing, malformed, or not ours.
    Untrusted,
}

/// The no-password repair for [`SigTrust::RetiredOnly`], named in one place
/// because three messages and a runbook have to agree on it.
///
/// **Not `veld update`**, and that was the first answer. `veld update` resolves a
/// target and takes its "already on the latest version" branch when there is
/// nothing newer, so the installer never runs — and a machine in the RetiredOnly
/// state is by construction *on* the latest release, because installing it is how
/// it got there. `--target-version` at the current version is the same no-op.
/// Re-running the installer does re-drive the helper handoff at the current
/// version, and the store's floor accepts an equal version, so this repairs it
/// and asks for no password.
pub const INSTALLER_COMMAND: &str = "curl -fsSL https://veld.oss.life.li/get | bash";

/// Keys that were once active and are not any more — [`ORG_REQUIRED_SLOT_KEYS`]
/// minus [`ORG_SIGNING_KEYRING`].
///
/// Derived rather than listed so it cannot disagree with the two lists it comes
/// from. Empty until the first retirement, which is why
/// [`SigTrust::RetiredOnly`] is unreachable today and tested with explicit lists
/// instead.
fn retired_keys() -> Vec<PubKey> {
    ORG_REQUIRED_SLOT_KEYS
        .iter()
        .filter(|key| !ORG_SIGNING_KEYRING.contains(key))
        .copied()
        .collect()
}

/// [`classify_binary_signature`] against explicit lists, so the retired-key case
/// is testable before any key has actually been retired.
pub fn classify_data(active: &[PubKey], retired: &[PubKey], data: &[u8], sig: &[u8]) -> SigTrust {
    classify_data_detail(active, retired, data, sig).trust
}

/// A [`SigTrust`] and, when one verified, the key that produced it.
///
/// One value rather than two calls, because classifying reads and hashes the whole
/// ~26 MB binary: asking a second time for the key would double that, and the two
/// answers can disagree if an update lands between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SigVerdict {
    pub trust: SigTrust,
    /// The key a slot verified under — public, and `None` for
    /// [`SigTrust::Untrusted`].
    pub key: Option<PubKey>,
}

/// [`classify_data`], naming the key that answered.
pub fn classify_data_detail(
    active: &[PubKey],
    retired: &[PubKey],
    data: &[u8],
    sig: &[u8],
) -> SigVerdict {
    if let Some(key) = verifying_key(active, data, sig) {
        SigVerdict {
            trust: SigTrust::Active,
            key: Some(key),
        }
    } else if let Some(key) = verifying_key(retired, data, sig) {
        SigVerdict {
            trust: SigTrust::RetiredOnly,
            key: Some(key),
        }
    } else {
        SigVerdict {
            trust: SigTrust::Untrusted,
            key: None,
        }
    }
}

/// How much this build trusts `binary`'s detached signature.
///
/// **A verification failure is re-checked once before it is believed** — and only
/// that failure, which is the narrow part.
///
/// The signature and the binary are read in two separate operations, and
/// `helper_store` installs the signature *then* the binary, so a read landing
/// between the two renames sees a new signature beside a stale binary and nothing
/// verifies. That is fail-closed and self-healing (no tear can produce a false
/// `Active`), but the caller's reaction to `Untrusted` is a paragraph about a
/// tampered root binary, and printing that on a machine where an update has just
/// landed correctly is the alarm [`SigTrust::RetiredOnly`] exists to avoid
/// elsewhere.
///
/// **The retry is scoped to the tear, not to every refusal.** A tear means both
/// reads *succeeded* and the bytes did not verify, so an unreadable path — a
/// missing file, a FIFO, a device node, an over-bound binary — is answered once and
/// not retried. The first version retried every refusal, which handed a second
/// race per watcher tick against the pre-open `stat` in
/// [`read_regular_file_bounded`] to any machine whose helper path was merely
/// *unreadable* — permanently, and on machines nobody had attacked.
///
/// Narrowly stated, because an earlier wording here overclaimed twice. The scoping
/// removes the doubling only for **benign** refusals: somebody who can already
/// write the directory still arms two windows per tick by leaving a *readable*
/// non-verifying pair there, which is the state they are in anyway once they have
/// swapped the binary.
///
/// And the doubled thing is worse than an `open` race, which is what the earlier
/// wording said. `relaunch_guard` returning `None` is what makes the helper
/// `exit(0)` so the service manager re-execs whatever is at the path — so a second
/// pass is a second **chance to be seen as genuine before that exec**, to an
/// attacker alternating a real release pair with their own binary. It does not
/// change reachability (the watcher ticks forever either way) and it is strictly
/// fewer windows than the version it replaced, but it is two per tick, not one.
///
/// It **reduces** the false alarm rather than removing it, and the arithmetic is
/// worth stating rather than overclaiming: the retry is immediate, and one pass over
/// a release helper is a few tens of milliseconds against a window that includes
/// writing and `fsync`ing the whole binary. A second pass can land inside the same
/// window. `veld doctor` additionally knows when an update is in progress and is the
/// right place to suppress the paragraph outright if this proves not enough.
pub fn classify_binary_signature(binary: &Path) -> SigTrust {
    classify_binary_signature_detail(binary).trust
}

/// [`classify_binary_signature`], naming the key that answered.
///
/// The one caller is `veld doctor`'s signature row. Everything that *gates* on a
/// signature wants the verdict alone and takes [`classify_binary_signature`].
pub fn classify_binary_signature_detail(binary: &Path) -> SigVerdict {
    classify_with_retry(|| classify_binary_signature_once(binary, MAX_VERIFIED_BINARY_BYTES))
}

/// [`classify_binary_signature`]'s retry rule, over a pass it can count.
///
/// Split out for the same reason [`any_slot_verifies`] is: the scoping changes no
/// verdict any other test can observe, only the number of passes — so without a
/// seam to count them, "retry only the tear" could be simplified back to "retry
/// every refusal" with nothing red anywhere, handing an attacker a second race per
/// tick against the pre-`stat` in [`read_regular_file_bounded`].
/// `only_a_verification_failure_is_retried` counts them.
fn classify_with_retry(mut pass: impl FnMut() -> Option<SigVerdict>) -> SigVerdict {
    let untrusted = SigVerdict {
        trust: SigTrust::Untrusted,
        key: None,
    };
    match pass() {
        // A read failed. Answer once; do not hand out a second race.
        None => untrusted,
        Some(v) if v.trust == SigTrust::Untrusted => pass().unwrap_or(untrusted),
        Some(settled) => settled,
    }
}

/// One pass of [`classify_binary_signature`]. `None` means a read failed, which is
/// what separates a tear from an unreadable path — see there.
fn classify_binary_signature_once(binary: &Path, limit: u64) -> Option<SigVerdict> {
    let sig = match read_detached_sig_slots_classified(binary) {
        SigRead::Slots(slots) => slots,
        // Read fine, no whole slot: a **torn** `.sig`, which is what the retry is
        // for. `install.sh` writes the lib-dir copy with `cp`, so the file really
        // does pass through zero length in place. Reporting this as a read failure
        // put it on the wrong side of the rule — see [`SigRead`].
        SigRead::NoWholeSlot => {
            return Some(SigVerdict {
                trust: SigTrust::Untrusted,
                key: None,
            });
        }
        SigRead::Unreadable => return None,
    };
    // Read through the **same** shape as the `.sig` above, and that pairing is the
    // point: a first attempt at this guard used `std::fs::metadata` plus
    // `std::fs::read`, which is the weaker half of the pair. `std::fs::read` has no
    // `O_NONBLOCK`, so winning the race with a symlink to a FIFO parks root's open
    // **forever** — an uncancellable stall of the helper's binary watcher or its
    // `shutdown` handler, which is the repair channel itself.
    //
    // Reachable, and not only through the install RPC. On a privileged install not
    // yet migrated to the root-owned store the installing user owns
    // `~/.local/lib/veld/veld-helper`, so they can replace it with a symlink and
    // send the uid-gated `shutdown` request — which calls the relaunch guard
    // **directly**, with no `binary_executes()` in front of it, unlike the watcher
    // and `restart`.
    // `limit` is a parameter rather than the constant, so the over-bound arm below
    // has a test seam. Without one, deleting that arm leaves every test green while
    // the binary is verified as a prefix — reinstating the slots × keys hashing cost
    // [`MAX_SIG_SLOTS`] spends a paragraph bounding. The same argument that gave
    // [`any_slot_verifies`] and [`classify_with_retry`] theirs.
    let (data, over) = read_regular_file_bounded(binary, limit)?;
    if over {
        // A helper bigger than the bound is not one of ours. Fail closed rather
        // than verify a prefix, which would verify nothing — and report it as a
        // read failure so it is not retried.
        return None;
    }
    Some(classify_data_detail(
        ORG_SIGNING_KEYRING,
        &retired_keys(),
        &data,
        &sig,
    ))
}

/// Whether `binary` carries a valid org signature in `<binary>.sig`.
///
/// `false` for any failure — see the module doc on fail-closed. A signature by a
/// **retired** key is a failure here too: admitting one would make retirement mean
/// nothing.
///
/// **This is the strict predicate by name, not a live gate**, and saying so is the
/// honest version of a claim this comment used to make. It has no production
/// callers: the relaunch gate is [`relaunch_guard`], the install gate is
/// `helper_store::Candidate::verified_version_with` (which hands
/// [`ORG_SIGNING_KEYRING`] to [`verify_data_slots`] directly), and `setup`'s two
/// service-definition call sites use [`is_org_binary`]. It stays because it is the
/// name a reader looks for, and because it and [`is_org_binary`] only make sense as
/// a documented pair — which is the mitigation for the one mistake in this module
/// that would be a privilege escalation rather than a brick: calling the lax one
/// where the strict one belongs.
pub fn verify_binary_signed(binary: &Path) -> bool {
    classify_binary_signature(binary) == SigTrust::Active
}

/// The refusal text for a binary signed only by a retired key.
///
/// Split out so the test that holds its remedy honest reads the real string
/// rather than a copy: the advice was wrong once (`veld update`, which is a no-op
/// on every machine that can reach this state) and both `SigTrust` arms are
/// unreachable in this build, so nothing else would catch it.
fn relaunch_guard_message_for_retired(binary: &Path) -> String {
    format!(
        "the binary at {} is signed with an org key this release has retired, and its {} \
         carries no slot for a current one; refusing to relaunch onto it. The binary itself is \
         genuine — re-run the installer ({INSTALLER_COMMAND}) to write the full signature back; \
         no password is needed, and `veld update` will not do it when you are already on the \
         latest release",
        binary.display(),
        sig_path_for(binary).display()
    )
}

/// Whether `binary` is genuinely an org build, **whatever key generation signed
/// it** — including one this release has retired.
///
/// **A different question from [`verify_binary_signed`], and the difference is
/// the point.** That one answers "may I relaunch onto this, or install it", where
/// a retired key must not be enough or retirement would mean nothing. This one
/// answers "is this file ours", which is what
/// `setup::which_privileged_helper` and `setup::fallback_helper_path` actually
/// need: they are choosing which path to write into a **service definition**, and
/// their alternative when the answer is no is a bare `"veld-helper"` that launchd
/// cannot exec — a bricked root service.
///
/// Safe to be laxer here precisely because it is not a gate — but only for a file
/// **nobody but root can have written**, and that is now checked rather than
/// argued. The store is root-owned under a root-owned parent, so only root put
/// that file there, and it got there by passing the *strict* install gate of
/// whichever helper generation installed it. Refusing it would protect nothing an
/// attacker could reach; it would only brick the machine — and it would brick it
/// in the truncation window described on [`SigTrust::RetiredOnly`], where the
/// binary is beyond doubt genuine.
///
/// The ownership walk is what stops that argument from being the only thing
/// holding. If the invariant is ever broken — a failed migration, a restore from a
/// user-owned backup, a legacy `/usr/local` tree a Homebrew install owns — a
/// retired key stops being accepted here and this falls back to the strict answer.
/// An **active** signature is accepted unconditionally, so the anti-brick property
/// is untouched on every healthy machine.
///
/// **Do not use this for a relaunch, an install, or anything else that decides
/// whether to trust incoming bytes.** Those are [`verify_binary_signed`].
pub fn is_org_binary(binary: &Path) -> bool {
    let trust = classify_binary_signature(binary);
    // The ownership walk is only asked for when it can change the answer, so a
    // healthy machine pays nothing for it.
    let root_owned =
        || trust == SigTrust::RetiredOnly && crate::helper_store::is_root_owned_and_locked(binary);
    org_binary_verdict(trust, root_owned())
}

/// [`is_org_binary`]'s decision, as a pure function of its two inputs.
///
/// Split out because [`SigTrust::RetiredOnly`] is unreachable in a build with
/// nothing retired, so the only way to hold the retired arm honest is to test the
/// mapping directly — and this arm is the one place in the whole mechanism where a
/// retired key changes an outcome.
fn org_binary_verdict(trust: SigTrust, root_owned: bool) -> bool {
    match trust {
        SigTrust::Active => true,
        SigTrust::RetiredOnly => root_owned,
        SigTrust::Untrusted => false,
    }
}

/// A reason when `binary` is NOT safe to relaunch onto, or `None` when it
/// verifies. Wraps [`classify_binary_signature`] with a diagnosable message for
/// the fail-closed callers (the helper's watcher/`restart`/`shutdown`, and `veld
/// doctor`) — **not** [`verify_binary_signed`], which this said before and which
/// has no production callers at all: three verdicts need three messages, and a
/// bool cannot carry the retired-key one.
pub fn relaunch_guard(binary: &Path) -> Option<String> {
    match classify_binary_signature(binary) {
        SigTrust::Active => None,
        // Named separately because the remedy is different and the alarming
        // reading is wrong: these bytes are genuinely ours. See
        // [`SigTrust::RetiredOnly`] for how a healthy machine gets here.
        SigTrust::RetiredOnly => Some(relaunch_guard_message_for_retired(binary)),
        SigTrust::Untrusted => Some(format!(
            "the binary at {} is not signed with the org's key (or its {} is \
             missing/invalid); refusing to relaunch onto it",
            binary.display(),
            sig_path_for(binary).display()
        )),
    }
}

// ---------------------------------------------------------------------------
// Version attestation (#262)
// ---------------------------------------------------------------------------
//
// The install RPC must refuse an **older but genuinely signed** helper: the
// attacker is the installing user, so #253's uid gate admits them and they can
// hand the root helper a past release with a known vulnerability. It verifies
// perfectly, because a signature attests provenance, not currency.
//
// So the version has to come from *inside* the signed payload. It already does,
// and that is the whole trick: the signature covers every byte of the binary,
// and the binary can be made to carry a version record of our own design. No
// second signed document, no change to what `veld-sign` writes, no change to
// `release.yml`'s packaging — and therefore **no way for this to wedge the
// updater**, which is the one failure #338's rule 2 forbids. A manifest, a
// `version` field prepended to the `.sig`, or a per-release signing subkey all
// change what the *previous* release's `relaunch_guard` sees, and a previous
// release that refuses to relaunch onto the new binary is an update that can
// only be repaired with sudo.
//
// Rejected alternatives, so the next reader does not re-derive them:
//
//   * **Signed JSON manifest** (version + hash, signed alongside): the obvious
//     answer. Costs `veld-sign`, `release.yml` and #339's smoke test, and puts a
//     format change in the delivery path to buy a number the binary already has.
//   * **Exec the candidate with `--version` after verifying it.** Tempting,
//     because verification does dissolve the "self-reported" objection, and it is
//     the only mechanism that can read releases published before this one. But it
//     execs a known-vulnerable artifact — the exact class this exists to reject —
//     *before* deciding to reject it.
//   * **Blocklist of every prior release's hash.** Reads the immutable past
//     exactly, but its default is *accept*: any signed build missing from the
//     list is admitted forever, silently.

/// The version record's 16-byte magic, stored **obfuscated** and recovered by
/// XOR at both compile time and run time.
///
/// The plaintext magic must never exist as a standalone constant, and this is
/// not paranoia about secrecy — the magic is public. It is because **the scanner
/// and the scanned are the same program.** A helper links
/// [`version_in_signed_bytes`], and a *newer* helper will one day scan that
/// helper looking for this magic. If the plaintext sat in the scanner's own
/// rodata, every binary would carry two hits: its record, and its copy of the
/// needle.
///
/// Storing the halves separately and joining them at runtime was tried first and
/// is not enough: in a debug build the linker placed the two halves next to each
/// other and reproduced the exact byte sequence anyway
/// (`the_scanners_own_needle_is_not_a_second_record` is what found it). That
/// mitigation depended on layout; this one depends on arithmetic. The plaintext
/// appears only where [`version_record`] writes it.
const VERSION_MAGIC_OBFUSCATED: [u8; 16] = [
    0xff, 0xa3, 0x29, 0x7e, 0x79, 0x3e, 0xed, 0xb2, 0x0f, 0x11, 0x20, 0xc7, 0xf7, 0x9c, 0x5c, 0xc2,
];

/// XOR key for [`VERSION_MAGIC_OBFUSCATED`]. Any non-zero value works; this one
/// is arbitrary and must not change once a release has shipped, because it
/// decides the magic every past helper is looking for.
const VERSION_MAGIC_KEY: u8 = 0xa5;

/// Bytes reserved for the version string inside a record. Fixed width so the
/// field has a known end without needing a terminator: rodata carries no NULs
/// between strings, so "read until the next NUL" would run into whatever the
/// linker placed next.
const VERSION_FIELD_LEN: usize = 32;

/// Total size of an embedded version record: 16 magic + 32 version.
pub const VERSION_RECORD_LEN: usize = 16 + VERSION_FIELD_LEN;

/// The plaintext magic. `const` so [`version_record`] can build it at compile
/// time, and used at runtime by [`version_in_signed_bytes`] — one definition,
/// both directions.
const fn version_record_magic() -> [u8; 16] {
    let mut magic = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        magic[i] = VERSION_MAGIC_OBFUSCATED[i] ^ VERSION_MAGIC_KEY;
        i += 1;
    }
    magic
}

/// Build the record a binary embeds so its version can be read back out of the
/// signed bytes: the magic, then `version` as ASCII, NUL-padded to a fixed width.
///
/// `const` so the result is a compile-time constant a `#[used] static` can hold —
/// the record has to be *data in the binary*, not something computed at startup,
/// or there would be nothing in the file to find.
///
/// Panics at compile time if `version` does not fit, which is the right moment:
/// a version too long to record is a build that must not ship.
pub const fn version_record(version: &str) -> [u8; VERSION_RECORD_LEN] {
    let mut out = [0u8; VERSION_RECORD_LEN];
    let magic = version_record_magic();
    let mut i = 0;
    while i < 16 {
        out[i] = magic[i];
        i += 1;
    }
    let bytes = version.as_bytes();
    assert!(
        bytes.len() <= VERSION_FIELD_LEN,
        "version string does not fit in the embedded version record"
    );
    let mut j = 0;
    while j < bytes.len() {
        out[16 + j] = bytes[j];
        j += 1;
    }
    out
}

/// The version recorded inside `bytes`, or `None` when there isn't exactly one
/// answer.
///
/// Callers must have verified `bytes` against the org key **first**. This
/// function trusts what it reads, and that trust is only earned by the
/// signature: nothing else stops a caller writing whatever version it likes into
/// a file it controls.
///
/// Every well-formed record found must agree. Requiring literally one *hit*
/// would be too strict — a macOS universal binary carries a slice per
/// architecture and therefore a record per slice, all saying the same thing —
/// and refusing that would wedge the updater on the very artifacts it exists to
/// install. Requiring one *value* keeps the strictness where it belongs.
pub fn version_in_signed_bytes(bytes: &[u8]) -> Option<String> {
    let magic = version_record_magic();
    let mut found: Option<String> = None;
    for start in 0..bytes.len().saturating_sub(VERSION_RECORD_LEN - 1) {
        if bytes[start..start + 16] != magic {
            continue;
        }
        let Some(version) = parse_version_field(&bytes[start + 16..start + VERSION_RECORD_LEN])
        else {
            // The magic without a well-formed field is not a record. It cannot
            // be forged into one without the org key, so skipping it is safe —
            // and treating it as fatal would let one unlucky byte sequence in a
            // dependency stop every install.
            continue;
        };
        match &found {
            Some(seen) if *seen != version => return None,
            Some(_) => {}
            None => found = Some(version),
        }
    }
    found
}

/// A record's fixed-width version field as a string: printable ASCII, then NUL
/// padding to the end, and non-empty.
fn parse_version_field(field: &[u8]) -> Option<String> {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    if end == 0 || field[end..].iter().any(|b| *b != 0) {
        return None;
    }
    let text = std::str::from_utf8(&field[..end]).ok()?;
    text.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .then(|| text.to_owned())
}

/// Whether `candidate` is at least as new as `running`, comparing the numeric
/// `MAJOR.MINOR.PATCH` triple.
///
/// Equal passes: an install RPC that refused the version already running could
/// not repair a corrupted helper, and re-installing the same release is exactly
/// what `veld update --target-version` and a re-run of `install.sh` do.
///
/// A version either side cannot be read as a triple returns `false` — fail
/// closed, like everything else here. A pre-release suffix (`1.2.3-rc1`) is
/// compared on the triple alone; veld does not ship them, and guessing an order
/// for them would be a worse answer than the conservative one.
pub fn version_is_not_older(candidate: &str, running: &str) -> bool {
    match (version_triple(candidate), version_triple(running)) {
        (Some(c), Some(r)) => c >= r,
        _ => false,
    }
}

/// `MAJOR.MINOR.PATCH` as a comparable tuple, ignoring any `-`/`+` suffix.
fn version_triple(version: &str) -> Option<(u64, u64, u64)> {
    let core = version
        .split_once(['-', '+'])
        .map_or(version, |(before, _)| before);
    let mut parts = core.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let triple = (next()?, next()?, next()?);
    parts.next().is_none().then_some(triple)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn keypair() -> (VerifyingKey, SigningKey) {
        let signing = SigningKey::from_bytes(&[9u8; 32]);
        (VerifyingKey::from(&signing), signing)
    }

    #[test]
    fn verify_data_accepts_a_valid_signature() {
        let (vk, signing) = keypair();
        let sig = signing.sign(b"the genuine binary");
        assert!(verify_data(
            &vk.to_bytes(),
            b"the genuine binary",
            &sig.to_bytes()
        ));
    }

    #[test]
    fn verify_data_rejects_tampered_data() {
        let (vk, signing) = keypair();
        let sig = signing.sign(b"genuine");
        assert!(!verify_data(&vk.to_bytes(), b"tampered", &sig.to_bytes()));
    }

    #[test]
    fn verify_data_rejects_wrong_key() {
        let (_, signing) = keypair();
        let other = SigningKey::from_bytes(&[8u8; 32]);
        let sig = signing.sign(b"data");
        assert!(!verify_data(
            &VerifyingKey::from(&other).to_bytes(),
            b"data",
            &sig.to_bytes()
        ));
    }

    #[test]
    fn verify_data_rejects_wrong_signature_length() {
        let (vk, _) = keypair();
        assert!(!verify_data(&vk.to_bytes(), b"data", b"tooshort"));
    }

    #[test]
    fn sig_path_appends_not_replaces() {
        assert_eq!(
            sig_path_for(Path::new("/x/veld-helper")),
            PathBuf::from("/x/veld-helper.sig")
        );
    }

    #[test]
    fn file_verification_passes_on_a_validly_signed_file() {
        let dir = std::env::temp_dir().join(format!("veld-signing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (vk, signing) = keypair();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"binary").unwrap();
        std::fs::write(sig_path_for(&binary), signing.sign(b"binary").to_bytes()).unwrap();
        assert!(verify_data(
            &vk.to_bytes(),
            b"binary",
            &std::fs::read(sig_path_for(&binary)).unwrap()
        ));

        // Tamper the binary → fails the content check.
        assert!(!verify_data(
            &vk.to_bytes(),
            b"tampered",
            &std::fs::read(sig_path_for(&binary)).unwrap()
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // -- Version attestation (#262) -----------------------------------------

    /// A record round-trips: what a binary embeds is what a scanner reads back.
    #[test]
    fn a_record_reads_back_as_the_version_it_was_built_from() {
        let record = version_record("16.58.3");
        // Buried in noise, the way it sits in a real binary.
        let mut bytes = vec![0x41u8; 500];
        bytes.extend_from_slice(&record);
        bytes.extend_from_slice(&[0x42u8; 500]);
        assert_eq!(version_in_signed_bytes(&bytes).as_deref(), Some("16.58.3"));
    }

    /// **The rollback this whole mechanism exists for.** An older release is
    /// genuinely signed and verifies perfectly; only the version stops it.
    #[test]
    fn an_older_version_is_refused_and_the_same_or_newer_is_not() {
        assert!(!version_is_not_older("16.57.0", "16.58.3"));
        assert!(!version_is_not_older("15.99.99", "16.0.0"));
        assert!(!version_is_not_older("16.58.2", "16.58.3"));
        // Equal is allowed: re-installing the running release is what a repair,
        // a `--target-version`, and a re-run of install.sh all do.
        assert!(version_is_not_older("16.58.3", "16.58.3"));
        assert!(version_is_not_older("16.58.4", "16.58.3"));
        assert!(version_is_not_older("17.0.0", "16.58.3"));
        // Ordering is numeric, not lexical — the case a string compare gets
        // wrong, and gets wrong in the direction that admits an older build.
        assert!(!version_is_not_older("16.9.0", "16.10.0"));
        assert!(version_is_not_older("16.10.0", "16.9.0"));
    }

    /// Anything unreadable as a triple fails closed, on either side.
    #[test]
    fn an_unparseable_version_is_refused_rather_than_guessed() {
        for (candidate, running) in [
            ("", "16.58.3"),
            ("sixteen", "16.58.3"),
            ("16.58", "16.58.3"),
            ("16.58.3.1", "16.58.3"),
            ("16.58.3", ""),
            ("16.58.3", "not-a-version"),
        ] {
            assert!(
                !version_is_not_older(candidate, running),
                "{candidate:?} over {running:?} should have been refused"
            );
        }
    }

    /// Bytes carrying no record read as no version — which the install path
    /// treats as a refusal, so a helper built before this mechanism existed can
    /// never be installed over one that has it.
    #[test]
    fn bytes_with_no_record_have_no_version() {
        assert_eq!(version_in_signed_bytes(&[0u8; 4096]), None);
        assert_eq!(version_in_signed_bytes(b"an older release"), None);
    }

    /// Two records that disagree are no answer at all.
    ///
    /// Nothing can forge a second record into a signed binary — that needs the
    /// org key — so this is not an attack being blocked. It is the honest
    /// reading of an ambiguous artifact, and the alternative (take the first)
    /// would let a future build shape silently decide the version by layout.
    #[test]
    fn disagreeing_records_read_as_no_version() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&version_record("16.58.3"));
        bytes.extend_from_slice(&[0u8; 64]);
        bytes.extend_from_slice(&version_record("1.0.0"));
        assert_eq!(version_in_signed_bytes(&bytes), None);
    }

    /// Identical records do *not* disagree.
    ///
    /// A macOS universal binary carries one slice per architecture and therefore
    /// one record per slice. Refusing that would wedge the updater on exactly the
    /// artifacts it exists to install, so "one value" is the rule rather than
    /// "one hit".
    #[test]
    fn repeated_identical_records_are_one_answer() {
        let mut bytes = Vec::new();
        for _ in 0..3 {
            bytes.extend_from_slice(&version_record("16.58.3"));
            bytes.extend_from_slice(&[0xcc; 32]);
        }
        assert_eq!(version_in_signed_bytes(&bytes).as_deref(), Some("16.58.3"));
    }

    /// The magic with a malformed field behind it is not a record.
    ///
    /// This is the tolerance that keeps an unlucky byte sequence in a dependency
    /// from stopping every install: it is skipped, not fatal.
    #[test]
    fn the_magic_without_a_well_formed_field_is_skipped() {
        let magic = version_record_magic();
        let mut bytes = Vec::new();
        // Magic followed by rodata-like text with no NUL padding — what a stray
        // occurrence in a real binary looks like.
        bytes.extend_from_slice(&magic);
        bytes.extend_from_slice(b"could not read own executable pa");
        assert_eq!(version_in_signed_bytes(&bytes), None);

        // ...and it does not stop a real record elsewhere being found.
        bytes.extend_from_slice(&version_record("16.58.3"));
        assert_eq!(version_in_signed_bytes(&bytes).as_deref(), Some("16.58.3"));
    }

    /// The plaintext magic must not be a constant anywhere in this crate's own
    /// data — see [`VERSION_MAGIC_OBFUSCATED`]. This asserts the arithmetic that
    /// replaced a layout assumption.
    #[test]
    fn the_magic_is_recovered_by_xor_and_not_stored_in_the_clear() {
        let magic = version_record_magic();
        assert_ne!(magic, VERSION_MAGIC_OBFUSCATED);
        for (i, byte) in magic.iter().enumerate() {
            assert_eq!(*byte, VERSION_MAGIC_OBFUSCATED[i] ^ VERSION_MAGIC_KEY);
        }
    }

    /// A FIFO where the signature should be is refused, not waited on.
    ///
    /// Without `O_NONBLOCK` this test hangs rather than fails — which is exactly
    /// what the helper's binary watcher, `veld doctor`, and a blocking-pool
    /// thread of the root daemon would do on a machine where somebody left one
    /// in the lib dir.
    #[test]
    fn a_fifo_in_place_of_a_signature_is_refused_without_blocking() {
        let dir = std::env::temp_dir().join(format!("veld-signing-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"binary").unwrap();
        let sig = sig_path_for(&binary);
        let c = std::ffi::CString::new(sig.to_str().unwrap()).unwrap();
        // SAFETY: a plain libc call with a valid NUL-terminated path.
        assert_eq!(unsafe { nix::libc::mkfifo(c.as_ptr(), 0o600) }, 0);

        assert!(!verify_binary_signed(&binary));
        assert!(relaunch_guard(&binary).is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // -- Key rotation (#261 slice C) ----------------------------------------

    /// A named key generation, so the tests below read as "a G1 helper meets a
    /// G3 release" rather than as index arithmetic.
    fn gen_key(n: u8) -> (SigningKey, PubKey) {
        let signing = SigningKey::from_bytes(&[0xA0 + n; 32]);
        (signing.clone(), VerifyingKey::from(&signing).to_bytes())
    }

    /// A `.sig` as the release pipeline writes one: one 64-byte slot per key, in
    /// the order given, over exactly `data`.
    fn slots(keys: &[&SigningKey], data: &[u8]) -> Vec<u8> {
        keys.iter().flat_map(|k| k.sign(data).to_bytes()).collect()
    }

    /// A [`SigVerdict`] carrying just a trust level.
    ///
    /// The retry rule and the once-pass both branch on the trust level alone —
    /// the key rides along for `veld doctor` and nothing else reads it — so the
    /// tests below say what they mean and let this fill in the rest.
    fn verdict(trust: SigTrust) -> SigVerdict {
        SigVerdict { trust, key: None }
    }

    /// **The compatibility contract, asserted rather than trusted.**
    ///
    /// Every helper shipped up to v16.59.0 reads exactly bytes `0..64` of the
    /// `.sig` and checks them against its single embedded key, which is
    /// [`ORG_KEY_1`]. Their code cannot be changed. So [`ORG_KEY_1`] must stay
    /// first in [`ORG_REQUIRED_SLOT_KEYS`] — the list `release.yml` signs in
    /// order — or the next release is one every one of those helpers refuses to
    /// relaunch onto, and sudo becomes the only repair channel.
    #[test]
    fn org_key_1_holds_slot_zero_forever() {
        assert_eq!(
            ORG_REQUIRED_SLOT_KEYS.first(),
            Some(&ORG_KEY_1),
            "ORG_KEY_1 must be the first key a release is signed by: every helper \
             shipped up to v16.59.0 verifies sig[0..64] against it and nothing else"
        );
    }

    /// The three derived views really are views of [`ORG_KEYS`].
    ///
    /// They used to be three hand-kept declarations that a rotation edited
    /// separately, and the whole point of the table is that they are not any more.
    /// A `const fn` that quietly dropped or duplicated a row would compile, and the
    /// symptom would be a release signed by a key no reader expects — so the
    /// derivations are re-done here the obvious way and compared.
    #[test]
    fn the_derived_views_match_the_table() {
        let all: Vec<PubKey> = ORG_KEYS.iter().map(|k| k.key).collect();
        assert_eq!(
            ORG_REQUIRED_SLOT_KEYS, all,
            "every row of ORG_KEYS gets a slot, in table order"
        );
        // Structural, not a check: `RELEASE_SIG_SLOTS` **is** `ORG_KEYS.len()`, so
        // this cannot fail. Kept as an executable statement of the property, which
        // the deleted `release_slot_count_matches_the_key_lists` had to assert for
        // real when the count was a hand-written number beside two independent
        // lists. Written down because an assertion that cannot fail reads as a
        // check, and the next reader deserves to know which it is.
        const _: () = assert!(RELEASE_SIG_SLOTS == ORG_KEYS.len());

        let accepted: Vec<PubKey> = ORG_KEYS
            .iter()
            .filter(|k| k.status == KeyStatus::Accepted)
            .map(|k| k.key)
            .collect();
        assert_eq!(
            ORG_SIGNING_KEYRING, accepted,
            "the keyring is exactly the accepted rows, in table order"
        );
    }

    /// **The Python parser and the compiler agree about `ORG_KEYS`.**
    ///
    /// `tests/signing-slots.py` reads this table with regexes so that
    /// `release.yml` and `ci.yml` — neither of which can evaluate Rust — can know
    /// what a release must be signed with. That makes it a **second
    /// implementation** of a value the compiler already has, and the only thing
    /// standing between the two is this test. Comparing the script's two output
    /// modes against each other, which is what `ci.yml` used to do here, is
    /// tautological: the same parse produces both, so a parse that drops a row
    /// drops it identically on both sides.
    ///
    /// What that would cost, concretely: a row the parser misses is a slot the
    /// release does not carry, while `ORG_REQUIRED_SLOT_KEYS` says it does. One
    /// release later the generation whose keyring holds only that key finds no slot
    /// that verifies, refuses every candidate, and needs sudo on every machine.
    ///
    /// Skipped rather than failed when `python3` is not on `PATH`, because a
    /// missing interpreter is not a disagreement — CI has one, and `ci.yml`'s
    /// slot-layout gate runs the script directly and fails loudly if it cannot.
    #[test]
    fn the_slot_script_reads_the_same_table_the_compiler_does() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root is two levels above this crate")
            .to_path_buf();
        let out = match std::process::Command::new("python3")
            .arg("tests/signing-slots.py")
            .arg("--tsv")
            .current_dir(&root)
            .output()
        {
            Ok(out) => out,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Said out loud. A silent `ok` here is indistinguishable from
                // having run, and this is the only comparison between the script
                // and what rustc compiles. CI always has python3, and `ci.yml`'s
                // slot-layout gate runs the script directly anyway.
                eprintln!(
                    "SKIPPED the_slot_script_reads_the_same_table_the_compiler_does: \
                     no python3 on PATH, so the parser was NOT compared against ORG_KEYS"
                );
                return;
            }
            Err(e) => panic!("could not run tests/signing-slots.py: {e}"),
        };
        assert!(
            out.status.success(),
            "tests/signing-slots.py --tsv failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tsv = String::from_utf8(out.stdout).expect("the script prints UTF-8");
        let rows: Vec<Vec<&str>> = tsv
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.split('\t').collect())
            .collect();

        assert_eq!(
            rows.len(),
            ORG_KEYS.len(),
            "tests/signing-slots.py sees {} row(s) where rustc sees {}. A row the \
             script misses is a slot the release does not carry, and one release \
             later the generation that accepts only that key refuses everything. \
             Its output was:\n{tsv}",
            rows.len(),
            ORG_KEYS.len()
        );
        for (i, (row, key)) in rows.iter().zip(ORG_KEYS).enumerate() {
            let hex: String = key.key.iter().map(|b| format!("{b:02x}")).collect();
            let status = match key.status {
                KeyStatus::Accepted => "accepted",
                KeyStatus::Retired { .. } => "retired",
            };
            assert_eq!(
                row.as_slice(),
                [hex.as_str(), key.secret, key.added_after, status],
                "row {i}: the script and the compiler disagree about this key"
            );
        }
    }

    /// Every row names a distinct key and a distinct secret.
    ///
    /// Two rows sharing a key would be a release claiming to cover two helper
    /// generations while covering one — `veld-sign` refuses it outright, but at the
    /// release, which is push-only and therefore after merge. Two rows sharing a
    /// secret is the same mistake made by copy-paste: the second row's slot would be
    /// signed by the first row's key, which is exactly the invisible mis-pairing the
    /// `secret` field exists to prevent.
    #[test]
    fn no_key_and_no_secret_appears_twice() {
        for (i, a) in ORG_KEYS.iter().enumerate() {
            for b in &ORG_KEYS[i + 1..] {
                assert_ne!(a.key, b.key, "ORG_KEYS lists the same public key twice");
                assert_ne!(
                    a.secret, b.secret,
                    "two rows of ORG_KEYS name the same secret ({}), so one slot \
                     would be signed by the other row's key",
                    a.secret
                );
            }
        }
    }

    /// Every secret name carries the prefix the leak gate matches on.
    ///
    /// `ci.yml` proves no pull-request-startable job can reach a signing secret by
    /// matching that prefix across every workflow. That only works if the names
    /// actually carry it, which used to be prose in a runbook and is now a property
    /// of the table the workflow reads.
    #[test]
    fn every_secret_name_carries_the_signing_prefix() {
        for k in ORG_KEYS {
            assert!(
                k.secret.starts_with("SIGNING_"),
                "{} does not carry the shared signing-secret prefix; ci.yml's leak \
                 gate matches on it, so a name without it is a secret that can reach \
                 a pull-request job unnoticed",
                k.secret
            );
        }
    }

    /// **The two-release rule, as a test rather than as a warning in a runbook.**
    ///
    /// Adding a key and retiring one must be two separate releases. **What a
    /// release that does both actually costs**, since getting this wrong is how the
    /// rule gets talked past: every helper still *accepts* it, because the retired
    /// key keeps its slot. But a helper shipped up to v16.59.0 keeps only the first
    /// 64 bytes of the `.sig` when it installs, so it writes the retired key's slot
    /// into its store and comes up running a build whose keyring no longer holds
    /// that key — [`SigTrust::RetiredOnly`]: `restart` and `shutdown` refused,
    /// updates still working, no password, healed by the next release.
    ///
    /// Splitting the rotation removes the window entirely, because the adding
    /// release's own keyring still holds the old key.
    /// `a_combined_release_is_a_truncation_window_not_a_stranding` asserts all of
    /// it, so this comment cannot drift back into the version that said "sudo".
    ///
    /// Nothing used to fail a pull request that did both. It looked like a property
    /// of the *diff* — removing a key from a flat list destroys, from the resulting
    /// tree, every trace that the key was ever there — and the tripwire that
    /// accidentally covered it (`nothing_is_retired_yet`) would have expired the
    /// first time anything was ever retired.
    ///
    /// **What makes it a property of one file instead:** both halves of the edit
    /// record `Cargo.toml`'s version at the moment they were written, and
    /// `Cargo.toml` holds the *previous* release until semantic-release bumps it
    /// after the merge. So the two halves of a combined edit necessarily carry the
    /// **same** version — the one this tree shows right now — while two edits made
    /// in two releases cannot. No git history, no merge base, no network: the whole
    /// check is this tree.
    ///
    /// **This is one of two layers, and it is the weaker one.** It catches the
    /// combined edit written in one sitting, which is the common shape, and it does
    /// so with no git, no network and no CI — so it also covers a direct push to
    /// `main`. It does **not** catch a branch whose `Cargo.toml` moved between the
    /// two edits: those two dates are both honest and both differ from the current
    /// version, and no test reading only this file can tell that apart from a
    /// legitimate retirement. `ci.yml`'s *One release never both adds a key and
    /// retires one* reads the pull request's diff and closes exactly that; see the
    /// note on [`KeyStatus::Retired`].
    ///
    /// `added_after_increases_down_the_table` sits beside this one, refusing row 0's
    /// `0.0.0` and every earlier row's version as a value for a new row — the stale
    /// values that are actually lying around to copy.
    #[test]
    fn one_release_never_both_adds_a_key_and_retires_one() {
        let now = env!("CARGO_PKG_VERSION");
        let added: Vec<&str> = ORG_KEYS
            .iter()
            .filter(|k| k.added_after == now)
            .map(|k| k.secret)
            .collect();
        let retired: Vec<&str> = ORG_KEYS
            .iter()
            .filter(|k| {
                matches!(k.status, KeyStatus::Retired { retired_after } if retired_after == now)
            })
            .map(|k| k.secret)
            .collect();
        assert!(
            added.is_empty() || retired.is_empty(),
            "this release adds {added:?} and retires {retired:?}, and it may not do \
             both. Every helper would still accept it — the retired key keeps its \
             slot — but a helper shipped up to v16.59.0 keeps only the first 64 bytes \
             of the .sig when it installs, so it would come up on a store it cannot \
             verify: restart and shutdown refused until the installer is re-run. \
             Splitting the rotation removes that window, because the adding release \
             still accepts the old key. Ship the adding release first, then retire in \
             a second one. See docs/signing-key-rotation.md"
        );
    }

    /// A key's position is its row, 1-based, and every row has one.
    ///
    /// `veld doctor` prints this number, so it is a string a user pastes into an
    /// issue and somebody reads a year later. It only means anything because rows
    /// are append-only — nothing else pins that beyond row 0
    /// (`org_key_1_holds_slot_zero_forever`), so a future edit that reordered the
    /// table would silently renumber every key in every report already written.
    #[test]
    fn a_keys_position_is_its_row_and_never_moves() {
        for (i, k) in ORG_KEYS.iter().enumerate() {
            assert_eq!(
                org_key_position(&k.key),
                Some((i + 1, ORG_KEYS.len())),
                "{} is row {i}, so it is org key {} of {}",
                k.secret,
                i + 1,
                ORG_KEYS.len()
            );
        }
        // A key this build does not carry has no position rather than a wrong one.
        assert_eq!(org_key_position(&[0xAB; 32]), None);
    }

    /// `added_after` increases strictly down the table.
    ///
    /// **This is what stops the two-release guard being defeated by a paste.** That
    /// guard compares each row's date against `Cargo.toml`'s version, so any *stale*
    /// value in a newly added row slips past it — and the two most available stale
    /// values are sitting in the table being edited: row 0's `0.0.0`, and the
    /// previous row's version. Requiring each row to be strictly later than the one
    /// above refuses both, which leaves no value a hurried operator can copy.
    ///
    /// Strict rather than non-decreasing: two keys added in one release is two
    /// rotations at once, which the runbook does not describe and nobody should do
    /// by accident. It costs nothing to forbid and it closes the equal-to-previous
    /// paste.
    #[test]
    fn added_after_increases_down_the_table() {
        for pair in ORG_KEYS.windows(2) {
            let (prev, next) = (&pair[0], &pair[1]);
            // Parsed before comparing. `Option` orders `None < Some(_)`, so
            // comparing them directly makes an unparseable value in the *earlier*
            // row pass vacuously, and an unparseable one in the *later* row fail
            // with a message about ordering rather than about parsing.
            // `every_key_lifecycle_date_names_a_release_that_has_happened` is what
            // rejects the unparseable value itself; this just refuses to guess.
            let (Some(a), Some(b)) = (
                version_parts(prev.added_after),
                version_parts(next.added_after),
            ) else {
                continue;
            };
            assert!(
                b > a,
                "{}'s added_after ({:?}) is not later than {}'s ({:?}). Rows are \
                 appended in order, so each one was written against a later release \
                 than the one above it. If you copied this value from another row or \
                 from row 0's 0.0.0, that is the paste this check exists to refuse — \
                 use Cargo.toml's version",
                next.secret,
                next.added_after,
                prev.secret,
                prev.added_after
            );
        }
    }

    /// Every secret name fits the permanent roster in `release.yml`.
    ///
    /// That roster is `SIGNING_PRIVATE_KEY` plus `_2`..`_8`, written once and never
    /// edited — which is the whole promise that rotating needs no workflow change.
    /// A name outside it (`SIGNING_KEY_2026`, say) passes the prefix rule, then
    /// fails `ci.yml` with "add an entry to that step's env:" — telling the operator
    /// to do the one thing the runbook says they never have to. Caught here instead,
    /// where the name is being chosen.
    #[test]
    fn every_secret_name_is_one_of_the_permanent_roster() {
        for (i, k) in ORG_KEYS.iter().enumerate() {
            let expected = if i == 0 {
                "SIGNING_PRIVATE_KEY".to_owned()
            } else {
                format!("SIGNING_PRIVATE_KEY_{}", i + 1)
            };
            assert_eq!(
                k.secret, expected,
                "ORG_KEYS row {i} names {:?}, but release.yml's permanent roster \
                 calls that slot {expected:?}. Names are positional: the roster is \
                 written once to the format's slot ceiling and never edited, so a \
                 row must take the name for its own position",
                k.secret
            );
        }
    }

    /// Neither half of that rule may cite a release that has not happened.
    ///
    /// The fields are `Cargo.toml`'s version at the time of the edit, so a value
    /// *newer* than this tree is not a mistake with a benign reading — it is the
    /// shape a combined edit takes when somebody guesses at the version their merge
    /// will ship as, which is exactly what the rule above is stopping.
    #[test]
    fn every_key_lifecycle_date_names_a_release_that_has_happened() {
        let now = version_parts(env!("CARGO_PKG_VERSION")).expect("our own version parses");
        for k in ORG_KEYS {
            let mut dates = vec![("added_after", k.added_after)];
            if let KeyStatus::Retired { retired_after } = k.status {
                dates.push(("retired_after", retired_after));
                // Both parsed first, for the same reason as above: an unparseable
                // value must be diagnosed by the loop below, which says what is
                // wrong with it, not here with "older than", which is not.
                if let (Some(r), Some(a)) =
                    (version_parts(retired_after), version_parts(k.added_after))
                {
                    assert!(
                        r >= a,
                        "{}: retired_after {retired_after:?} is older than added_after {:?}",
                        k.secret,
                        k.added_after
                    );
                }
            }
            for (field, value) in dates {
                let parsed = version_parts(value).unwrap_or_else(|| {
                    panic!(
                        "{}'s {field} is {value:?}, which is not an x.y.z version. It is \
                         Cargo.toml's version at the moment you made the edit — copy it \
                         verbatim",
                        k.secret
                    )
                });
                assert!(
                    parsed <= now,
                    "{}'s {field} is {value:?}, which is newer than this tree ({}). These \
                     fields record the release an edit was made AGAINST, and a pull \
                     request cannot know the version it will itself ship as",
                    k.secret,
                    env!("CARGO_PKG_VERSION")
                );
            }
        }
    }

    /// `x.y.z` as a comparable tuple, or `None` if it is not that shape.
    fn version_parts(v: &str) -> Option<(u64, u64, u64)> {
        let mut it = v.split('.');
        let mut next = || it.next()?.parse::<u64>().ok();
        let parsed = (next()?, next()?, next()?);
        it.next().is_none().then_some(parsed)
    }

    /// **This is the load-bearing test of the whole slice.**
    ///
    /// The claim the format change rests on is that a helper *already in the
    /// field* still verifies a multi-slot release. That helper's code cannot be
    /// re-run here, so the test re-implements it: `read_exact` into a `[u8; 64]`,
    /// then one `verify_data` against one key. If a future edit to the writer
    /// ever moves [`ORG_KEY_1`]'s signature off byte 0, or pads before it, this
    /// fails — and what it is standing in for is every privileged install in
    /// existence refusing the release that was meant to move it forward.
    #[test]
    fn the_pre_rotation_reader_still_accepts_a_multi_slot_release() {
        let (k1, p1) = gen_key(1);
        let (k2, _) = gen_key(2);
        let (k3, _) = gen_key(3);
        let binary = b"\x7fELF a three-generation release";

        for keys in [vec![&k1], vec![&k1, &k2], vec![&k1, &k2, &k3]] {
            let sig = slots(&keys, binary);

            // Exactly what the shipped `read_detached_sig` does: a fixed 64-byte
            // buffer filled by `read_exact`, and everything after it unread.
            let mut first_slot = [0u8; 64];
            let mut cursor: &[u8] = &sig;
            cursor.read_exact(&mut first_slot).expect(
                "a release must always carry at least one whole slot; a shorter \
                 .sig is what `read_exact` fails closed on",
            );

            assert!(
                verify_data(&p1, binary, &first_slot),
                "a {}-slot release is not accepted by a pre-rotation helper",
                keys.len()
            );
        }
    }

    /// A one-key `.sig` is exactly the 64 bytes the pre-rotation reader expects.
    ///
    /// A permanent invariant about the *format*, not about today's key count: it
    /// stays true after every rotation and must never be deleted.
    #[test]
    fn a_one_key_signature_is_exactly_64_bytes() {
        let (k1, _) = gen_key(1);
        assert_eq!(slots(&[&k1], b"payload").len(), SIG_SLOT_LEN);
        assert_eq!(SIG_SLOT_LEN, 64);
    }

    /// The artifact carries exactly one slot per row of [`ORG_KEYS`], and nothing
    /// else.
    ///
    /// **This replaces a tripwire, and the replacement is the point.** The previous
    /// version asserted `RELEASE_SIG_SLOTS * SIG_SLOT_LEN == 64` — i.e. that no
    /// rotation had happened yet — and its own doc comment told the operator to
    /// delete it on the release that rotates. A procedure whose step 2 is "delete
    /// this red test" is a procedure that rehearses deleting red tests on the worst
    /// day available, next to two permanent invariants that look identical when the
    /// whole file is red.
    ///
    /// What it was actually protecting is that the signing artifact's shape does not
    /// change by accident, and that is a *relation* between the writer and the
    /// table — true before any rotation, true after every one, and red for a
    /// writer that pads, reorders or drops a slot. So it never needs deleting
    /// again.
    #[test]
    fn the_artifact_carries_exactly_one_slot_per_key() {
        let keys: Vec<_> = (0..ORG_KEYS.len())
            .map(|i| gen_key(i as u8 + 1).0)
            .collect();
        let refs: Vec<_> = keys.iter().collect();
        assert_eq!(
            slots(&refs, b"payload").len(),
            ORG_KEYS.len() * SIG_SLOT_LEN,
            "a release must carry one 64-byte slot per row of ORG_KEYS, with no \
             padding and no holes: slot position is this format's only key \
             identifier"
        );
    }

    /// **The acceptance criterion.** A helper trusting only the current key
    /// accepts a release that is *also* signed under a new key, and a helper that
    /// has moved to the new key accepts the same artifact — one release satisfies
    /// both, which is what makes a transition possible with no user action.
    #[test]
    fn one_release_satisfies_every_generation_that_signed_it() {
        let (k1, p1) = gen_key(1);
        let (k2, p2) = gen_key(2);
        let (k3, p3) = gen_key(3);
        let binary = b"\x7fELF the transition release";
        let sig = slots(&[&k1, &k2, &k3], binary);

        // G1 trusts only K1; G2 has accumulated K1+K2; G3 has retired K1.
        for (label, keyring) in [
            ("G1 (K1 only)", vec![p1]),
            ("G2 (K1 + K2)", vec![p1, p2]),
            ("G3 (K2 + K3, K1 retired)", vec![p2, p3]),
            ("G4 (K3 only)", vec![p3]),
        ] {
            assert!(
                verify_data_slots(&keyring, binary, &sig),
                "{label} refused a release it was signed for"
            );
        }
    }

    /// **The acceptance criterion, in the direction that matters.** A rotation
    /// artifact not signed by a currently-trusted key is refused — including one
    /// signed perfectly well by a key this generation has retired, which is the
    /// whole point of retiring it.
    #[test]
    fn a_release_signed_by_no_trusted_key_is_refused() {
        let (k1, p1) = gen_key(1);
        // Only the public half of K2 is needed: this test never signs with it —
        // that is the point, a generation that has moved to K2 must refuse a
        // signature it did not make.
        let (_, p2) = gen_key(2);
        let (attacker, _) = gen_key(9);
        let binary = b"\x7fELF not for you";

        // The forgery an attacker holding the leaked K1 can produce. A generation
        // that still trusts K1 takes it — that generation is unprotectable and
        // this states so — and a generation that has retired K1 does not.
        let leaked = slots(&[&k1], binary);
        assert!(verify_data_slots(&[p1], binary, &leaked));
        assert!(
            !verify_data_slots(&[p2], binary, &leaked),
            "retiring K1 must actually stop a K1 signature being sufficient — \
             otherwise rotation buys nothing at all"
        );

        // A key that was never ours is refused by everyone.
        let forged = slots(&[&attacker], binary);
        assert!(!verify_data_slots(&[p1, p2], binary, &forged));

        // And a slot list with no slot for the reader's key is refused even when
        // every slot in it is a genuine org signature.
        let genuine_but_wrong_generation = slots(&[&k1], binary);
        assert!(!verify_data_slots(
            &[p2],
            binary,
            &genuine_but_wrong_generation
        ));
    }

    /// A signature covers the bytes, so a slot list cannot be lifted onto other
    /// bytes — however many slots it has.
    #[test]
    fn slots_do_not_verify_against_bytes_they_do_not_cover() {
        let (k1, p1) = gen_key(1);
        let (k2, p2) = gen_key(2);
        let sig = slots(&[&k1, &k2], b"the genuine binary");
        assert!(verify_data_slots(&[p1, p2], b"the genuine binary", &sig));
        assert!(!verify_data_slots(&[p1, p2], b"a swapped binary", &sig));
    }

    /// **A retired row keeps its slot** — against a table that actually has one.
    ///
    /// This is the property everything about the two-release rule's *cost* rests
    /// on: because [`ORG_REQUIRED_SLOT_KEYS`] is every row and not just the
    /// accepted ones, a helper holding only the retired key still finds a slot it
    /// can verify, so a combined release degrades that machine rather than
    /// stranding it. See
    /// `a_combined_release_is_a_truncation_window_not_a_stranding`.
    ///
    /// **It could not be tested against the live table, and that is why this exists
    /// separately.** Today's `ORG_KEYS` has one row and it is accepted, so the
    /// keyring and the required list are the same list — replacing the required
    /// derivation with `ORG_SIGNING_KEYRING` outright left all 47 tests green. The
    /// property would have started being checked on the day of the first
    /// retirement, i.e. the day it first mattered, which is the same "test on a
    /// timer" defect as the store-path precondition in [`crate::paths`]. So the
    /// derivations take the table as a parameter and this hands them one with a
    /// retired row in the middle.
    #[test]
    fn the_derivation_keeps_a_retired_rows_slot() {
        const A: PubKey = [0xA1; 32];
        const B: PubKey = [0xB2; 32];
        const C: PubKey = [0xC3; 32];
        // Retired in the MIDDLE, so an off-by-one in the accepted filter shows up as
        // a wrong key rather than a short list.
        const TABLE: &[OrgKey] = &[
            OrgKey {
                key: A,
                secret: "SIGNING_PRIVATE_KEY",
                added_after: "0.0.0",
                status: KeyStatus::Accepted,
            },
            OrgKey {
                key: B,
                secret: "SIGNING_PRIVATE_KEY_2",
                added_after: "1.0.0",
                status: KeyStatus::Retired {
                    retired_after: "2.0.0",
                },
            },
            OrgKey {
                key: C,
                secret: "SIGNING_PRIVATE_KEY_3",
                added_after: "2.0.0",
                status: KeyStatus::Accepted,
            },
        ];

        assert_eq!(
            all_keys::<{ TABLE.len() }>(TABLE),
            [A, B, C],
            "every row gets a slot, retired ones included — this is what stops a \
             combined release stranding the generation that holds only the retired key"
        );
        assert_eq!(
            accepted_key_count(TABLE),
            2,
            "the retired row is not accepted"
        );
        assert_eq!(
            accepted_keys::<{ accepted_key_count(TABLE) }>(TABLE),
            [A, C],
            "the keyring is the accepted rows in table order, and skipping a retired \
             row must not shift a later key into its place"
        );
    }

    /// **What a combined add-and-retire release actually costs**, asserted rather
    /// than described — because the description was wrong, in the direction that
    /// makes somebody talk themselves past the guard.
    ///
    /// Three prose copies of this rule (including the failure message of
    /// `one_release_never_both_adds_a_key_and_retires_one`, and one that predates
    /// the key table) said such a release "strands every helper whose only key was
    /// the retired one — it refuses the very artifact meant to move it forward, and
    /// sudo on every machine is the only repair". **That is not what happens.** The
    /// retired key keeps its slot — [`ORG_REQUIRED_SLOT_KEYS`] is every row of
    /// [`ORG_KEYS`], retired ones included — so a helper holding only that key
    /// verifies slot 0 and installs the release quite happily.
    ///
    /// The real cost is narrower, and is the one `docs/signing-key-rotation.md`'s
    /// "Why retirement is a separate release" has always named: **the truncating
    /// installers.** A helper shipped up to v16.59.0 keeps only the first 64 bytes
    /// of the `.sig` when it installs, so it writes the retired key's slot into its
    /// store and then comes up running a build whose keyring no longer holds that
    /// key. That is [`SigTrust::RetiredOnly`] — `restart` and `shutdown` refused,
    /// updates still working, no password needed, and healed by the next release,
    /// whose install path keeps every slot.
    ///
    /// Worth a rule, and worth a guard. Not worth the word "sudo", which belongs to
    /// the case at the end of this test: dropping the row instead of retiring it,
    /// which really does leave that helper with nothing to verify.
    #[test]
    fn a_combined_release_is_a_truncation_window_not_a_stranding() {
        let (k1, p1) = gen_key(1);
        let (k2, p2) = gen_key(2);
        let binary = b"\x7fELF a release that both added and retired";

        // The combined release: keyring {K2}, K1 retired — but every row still gets
        // a slot, so K1's is still there.
        let sig = slots(&[&k1, &k2], binary);

        assert!(
            verify_data_slots(&[p1], binary, &sig),
            "a helper whose only key is the retired one ACCEPTS this release: the \
             retired key keeps its slot. Nothing is stranded, and no prose about this \
             rule may say otherwise"
        );

        // What the truncating installer leaves behind, and what the machine then
        // reads it as once it is running the combined release.
        let truncated = &sig[..SIG_SLOT_LEN];
        assert_eq!(
            classify_data(&[p2], &[p1], binary, truncated),
            SigTrust::RetiredOnly,
            "the cost of a combined release is the truncation window, not a refusal"
        );
        // Untruncated — every helper from v16.60.0 on — there is no window at all.
        assert_eq!(
            classify_data(&[p2], &[p1], binary, &sig),
            SigTrust::Active,
            "only the pre-v16.60.0 install path truncates, so only it pays this"
        );

        // And splitting the rotation is what removes the window: the adding
        // release's own keyring still holds the old key, so the surviving slot 0
        // verifies.
        assert_eq!(
            classify_data(&[p1, p2], &[], binary, truncated),
            SigTrust::Active,
            "the adding release accepts both keys, so a truncated store is fine — \
             this is the whole reason retirement is a second release"
        );

        // The genuinely fatal edit, for contrast: DROPPING the row rather than
        // retiring it stops producing the slot, and then that helper really does
        // refuse, with sudo as the only repair. `ci.yml`'s two-release step reports
        // that case separately, and its message is the one entitled to the word.
        let dropped = slots(&[&k2], binary);
        assert!(
            !verify_data_slots(&[p1], binary, &dropped),
            "with no slot for it, the helper holding only that key refuses"
        );
    }

    /// A retired key is **diagnosed**, never accepted.
    ///
    /// This is the state a machine reaches by jumping straight from a pre-rotation
    /// release to a post-retirement one: the old install RPC kept only the first
    /// 64 bytes of the `.sig`, so the store holds the right binary beside a
    /// signature with only the retired slot in it. Reporting that as "not signed
    /// with the org's key" would read as tampering on a machine where the bytes
    /// are genuine — but it must still not be *accepted*, or retirement would be
    /// decorative.
    #[test]
    fn a_retired_key_is_diagnosed_rather_than_accepted() {
        let (k1, p1) = gen_key(1);
        let (k2, p2) = gen_key(2);
        let binary = b"\x7fELF genuine, and truncated on the way in";

        let truncated = slots(&[&k1], binary);
        assert_eq!(
            classify_data(&[p2], &[p1], binary, &truncated),
            SigTrust::RetiredOnly
        );
        // Active wins when both would match, so a full slot list is never
        // reported as retired.
        let full = slots(&[&k1, &k2], binary);
        assert_eq!(classify_data(&[p2], &[p1], binary, &full), SigTrust::Active);
        // And nothing ours at all stays plain untrusted.
        let (attacker, _) = gen_key(9);
        assert_eq!(
            classify_data(&[p2], &[p1], binary, &slots(&[&attacker], binary)),
            SigTrust::Untrusted
        );
    }

    /// The keyring is never empty.
    ///
    /// **Do not delete this one on retirement day.** The runbook's retirement is
    /// one field — the last accepted row's `status`; doing that on a
    /// tree where step 2 was skipped, reverted or mis-merged ships a helper that
    /// trusts nothing — every candidate refused, every relaunch refused, `restart`
    /// and `shutdown` refused, and sudo the only repair. Which is the wedged
    /// updater #338's rule 2 forbids, shipped by the mechanism that exists to
    /// honour it.
    #[test]
    fn the_keyring_is_never_empty() {
        assert!(
            !ORG_SIGNING_KEYRING.is_empty(),
            "ORG_SIGNING_KEYRING is empty, so this build trusts nothing and refuses \
             every release. See docs/signing-key-rotation.md — a retirement removes \
             a key from the keyring only AFTER a release has added its successor"
        );
    }

    /// Every key this build accepts is one releases are actually signed by.
    ///
    /// **Do not delete this one on retirement day either.** A key in the keyring
    /// but not in `ORG_REQUIRED_SLOT_KEYS` is a key no release carries a slot for,
    /// so this build would refuse every release for a reason that looks like a
    /// signing failure.
    #[test]
    fn every_accepted_key_is_one_releases_are_signed_by() {
        for key in ORG_SIGNING_KEYRING {
            assert!(
                ORG_REQUIRED_SLOT_KEYS.contains(key),
                "a key this build accepts is not one releases are signed by, so \
                 this build would refuse every release"
            );
        }
    }

    /// A trailing partial slot is ignored rather than fatal, and a file with no
    /// whole slot at all is refused.
    ///
    /// The tolerant half is what keeps the format additive: a future release may
    /// append something this build does not understand, and this build must read
    /// noise that matches no key rather than failing closed on a genuine
    /// artifact. The strict half is the pre-rotation `read_exact` behaviour, kept.
    #[test]
    fn a_partial_slot_is_ignored_and_a_sub_slot_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("veld-sig-slots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (k1, p1) = gen_key(1);
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"payload").unwrap();

        // 64 bytes + 17 bytes of something else.
        let mut sig = slots(&[&k1], b"payload");
        sig.extend_from_slice(b"a future addition");
        std::fs::write(sig_path_for(&binary), &sig).unwrap();
        let read = read_detached_sig_slots(&binary).unwrap();
        assert_eq!(read.len(), 64, "the partial trailing slot must be dropped");
        assert!(verify_data_slots(&[p1], b"payload", &read));

        // 63 bytes: no whole slot.
        std::fs::write(sig_path_for(&binary), &sig[..63]).unwrap();
        assert!(read_detached_sig_slots(&binary).is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `is_org_binary` is "is this ours", where [`verify_binary_signed`] is "may I
    /// trust this". The asymmetry is the whole reason it exists: `setup`'s two
    /// callers are choosing a path to write into a service definition, and their
    /// alternative when the answer is no is a bare `"veld-helper"` launchd cannot
    /// exec.
    ///
    /// Only two of the three states are reachable in this build (nothing is
    /// retired yet), so the third is asserted on the mapping rather than on a
    /// file — which is the part a future edit could get wrong.
    #[test]
    fn is_org_binary_asks_whether_it_is_ours_not_whether_to_trust_it() {
        // The mapping over both inputs. A retired key is "ours" only for a file
        // nobody but root can have written — which is what bounds the laxity by
        // the property that makes it safe rather than by a comment claiming it.
        for (trust, root_owned, expected) in [
            (SigTrust::Active, true, true),
            // Active is accepted whatever the ownership: refusing it would brick
            // the root service on a machine where nothing is wrong.
            (SigTrust::Active, false, true),
            (SigTrust::RetiredOnly, true, true),
            (SigTrust::RetiredOnly, false, false),
            (SigTrust::Untrusted, true, false),
            (SigTrust::Untrusted, false, false),
        ] {
            assert_eq!(
                org_binary_verdict(trust, root_owned),
                expected,
                "{trust:?} with root_owned={root_owned} must be {expected}: \
                 `setup::fallback_helper_path` and `which_privileged_helper` write \
                 this answer into a root service definition, and a wrong `false` \
                 there is a job launchd cannot exec"
            );
        }
        assert_eq!(
            classify_data(
                &[],
                &[gen_key(1).1],
                b"genuine",
                &slots(&[&gen_key(1).0], b"genuine")
            ),
            SigTrust::RetiredOnly,
            "a retired key must classify as RetiredOnly, not Untrusted"
        );

        // And on a real file, for the two states this build can reach.
        let dir = std::env::temp_dir().join(format!("veld-sig-org-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"genuine bytes").unwrap();
        std::fs::write(
            sig_path_for(&binary),
            slots(&[&gen_key(9).0], b"genuine bytes"),
        )
        .unwrap();
        assert!(
            !is_org_binary(&binary),
            "a key that was never ours is not ours"
        );
        assert!(!verify_binary_signed(&binary));
        std::fs::write(sig_path_for(&binary), b"not even 64 bytes").unwrap();
        assert!(
            !is_org_binary(&binary),
            "an unreadable signature is not ours"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The retired-key refusal names a remedy that actually works.
    ///
    /// The first version said "run `veld update`", which is a **no-op** on the one
    /// machine that can reach this state: `veld update` resolves a target, finds
    /// nothing newer, and takes its "already on the latest version" branch without
    /// running the installer — and a RetiredOnly machine is on the latest release
    /// by construction, because installing it is how it got there. Both arms of
    /// this enum are unreachable in this build, so nothing else would ever have
    /// caught the wrong advice.
    #[test]
    fn the_retired_key_refusal_does_not_send_you_to_a_no_op() {
        let dir = std::env::temp_dir().join(format!("veld-sig-remedy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"payload").unwrap();
        std::fs::write(sig_path_for(&binary), slots(&[&gen_key(1).0], b"payload")).unwrap();

        // Reached through `relaunch_guard`'s own RetiredOnly arm, with explicit
        // lists, since nothing is retired in this build.
        let reason = match classify_data(
            &[],
            &[gen_key(1).1],
            b"payload",
            &slots(&[&gen_key(1).0], b"payload"),
        ) {
            SigTrust::RetiredOnly => relaunch_guard_message_for_retired(&binary),
            other => panic!("expected RetiredOnly, got {other:?}"),
        };
        assert!(reason.contains(INSTALLER_COMMAND), "{reason}");
        assert!(reason.contains("no password is needed"), "{reason}");
        // `veld update` may be *mentioned* — the message says explicitly that it
        // will not do this — but never in the imperative, which is the form the
        // wrong version used.
        for imperative in [
            "run `veld update`",
            "Run `veld update`",
            "with `veld update`",
        ] {
            assert!(
                !reason.contains(imperative),
                "the remedy must not be `veld update`: it does nothing on a machine \
                 already on the latest release, which is every machine that can reach \
                 this state.\n{reason}"
            );
        }
        assert!(reason.contains("will not do it"), "{reason}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A **torn** `.sig` is a verification failure, not a read failure — so it gets
    /// the retry, and an unreadable path still does not.
    ///
    /// The distinction is reachable rather than theoretical: `install.sh` writes the
    /// lib-dir `.sig` with `cp`, which truncates in place, so the file really does
    /// pass through zero length while the helper's watcher and `veld doctor` may be
    /// reading it. Classifying that as "unreadable" answered it once and printed the
    /// tampered-root-binary paragraph on a machine where nothing was wrong.
    #[test]
    fn a_torn_signature_is_retried_but_an_unreadable_one_is_not() {
        let dir = std::env::temp_dir().join(format!("veld-sig-torn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"payload").unwrap();

        // 0 and 63 bytes: read fine, no whole slot.
        for len in [0usize, 1, 63] {
            std::fs::write(sig_path_for(&binary), vec![0u8; len]).unwrap();
            assert!(
                matches!(
                    read_detached_sig_slots_classified(&binary),
                    SigRead::NoWholeSlot
                ),
                "{len} bytes is a torn write, not an unreadable path"
            );
            assert_eq!(
                classify_binary_signature_once(&binary, 4096).map(|v| v.trust),
                Some(SigTrust::Untrusted),
                "{len} bytes must be a verdict, so the retry applies"
            );
        }

        // Absent: genuinely unreadable, and answered once.
        std::fs::remove_file(sig_path_for(&binary)).unwrap();
        assert!(matches!(
            read_detached_sig_slots_classified(&binary),
            SigRead::Unreadable
        ));
        assert_eq!(classify_binary_signature_once(&binary, 4096), None);

        // A whole slot reads as slots, whatever it verifies as.
        std::fs::write(sig_path_for(&binary), slots(&[&gen_key(1).0], b"payload")).unwrap();
        assert!(matches!(
            read_detached_sig_slots_classified(&binary),
            SigRead::Slots(_)
        ));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The over-bound arm of `classify_binary_signature_once`, through its limit
    /// seam — a binary past the bound is a **read failure**, so it is refused and
    /// not retried.
    ///
    /// Without the seam the arm could be deleted with every test still green, and
    /// the binary would then be verified as a 128 MiB prefix.
    #[test]
    fn an_over_bound_binary_is_refused_once_and_not_retried() {
        let dir = std::env::temp_dir().join(format!("veld-sig-obb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        let body = vec![0x77u8; 4096];
        std::fs::write(&binary, &body).unwrap();
        std::fs::write(sig_path_for(&binary), slots(&[&gen_key(1).0], &body)).unwrap();

        assert_eq!(
            classify_binary_signature_once(&binary, 2048),
            None,
            "past the bound must be a read failure, so it is answered once"
        );
        // Under the bound the same pair is a verdict again — so the refusal is the
        // bound, not something else about the fixture.
        assert!(classify_binary_signature_once(&binary, 4096).is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Only a **verification failure** is retried, and the passes are counted.
    ///
    /// The scoping changes no verdict any other test observes — only how many times
    /// the two reads happen — so without this the loop could be simplified back to
    /// "retry every refusal" with nothing red, handing a second race per watcher
    /// tick to any machine whose helper path is merely unreadable.
    #[test]
    fn only_a_verification_failure_is_retried() {
        // A read failure is answered once.
        let mut calls = 0;
        assert_eq!(
            classify_with_retry(|| {
                calls += 1;
                None
            })
            .trust,
            SigTrust::Untrusted
        );
        assert_eq!(calls, 1, "an unreadable path must not get a second race");

        // A verification failure — both reads succeeded — is retried exactly once.
        let mut calls = 0;
        assert_eq!(
            classify_with_retry(|| {
                calls += 1;
                Some(verdict(SigTrust::Untrusted))
            })
            .trust,
            SigTrust::Untrusted
        );
        assert_eq!(calls, 2, "the tear is what the retry is for");

        // ...and the second pass is believed, which is the point.
        let mut calls = 0;
        assert_eq!(
            classify_with_retry(|| {
                calls += 1;
                if calls == 1 {
                    Some(verdict(SigTrust::Untrusted))
                } else {
                    Some(verdict(SigTrust::Active))
                }
            })
            .trust,
            SigTrust::Active
        );
        assert_eq!(calls, 2);

        // A settled verdict is never re-read, whichever it is.
        for trust in [SigTrust::Active, SigTrust::RetiredOnly] {
            let mut calls = 0;
            assert_eq!(
                classify_with_retry(|| {
                    calls += 1;
                    Some(verdict(trust))
                })
                .trust,
                trust
            );
            assert_eq!(calls, 1, "{trust:?} is settled on the first pass");
        }

        // A second pass that cannot read falls back to the refusal rather than
        // panicking on the `Option`.
        let mut calls = 0;
        assert_eq!(
            classify_with_retry(|| {
                calls += 1;
                if calls == 1 {
                    Some(verdict(SigTrust::Untrusted))
                } else {
                    None
                }
            })
            .trust,
            SigTrust::Untrusted
        );
        assert_eq!(calls, 2);
    }

    /// A binary past [`MAX_VERIFIED_BINARY_BYTES`] is refused rather than verified
    /// as a prefix — and refused as a *read failure*, so it is not retried.
    ///
    /// The read it replaced was unbounded, on a path an unprivileged caller can
    /// influence, and each slot verification hashes the whole thing. Verifying a
    /// prefix would verify nothing at all, so fail-closed is the only answer.
    #[test]
    fn a_binary_past_the_bound_is_refused_not_verified_as_a_prefix() {
        let dir = std::env::temp_dir().join(format!("veld-sig-bound-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (k1, p1) = gen_key(1);
        let binary = dir.join("veld-helper");

        // Just over a small bound, so the test costs bytes rather than gigabytes:
        // the reader is exercised through its own parameter.
        let body = vec![0x5Au8; 4096];
        std::fs::write(&binary, &body).unwrap();
        std::fs::write(sig_path_for(&binary), slots(&[&k1], &body)).unwrap();

        let (read, over) = read_regular_file_bounded(&binary, 2048).unwrap();
        assert!(over, "a file longer than the bound must report it");
        assert_eq!(read.len(), 2048, "and must be truncated to the bound");
        // The truncated prefix verifies under nothing, which is why a caller that
        // accepted it would be accepting an unverified binary.
        assert!(!verify_data_slots(&[p1], &read, &slots(&[&k1], &body)));

        // At the bound exactly: not over, and whole.
        let (read, over) = read_regular_file_bounded(&binary, 4096).unwrap();
        assert!(!over);
        assert_eq!(read, body);
        assert!(verify_data_slots(&[p1], &read, &slots(&[&k1], &body)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A **binary** symlinked to a non-regular file is refused, like the `.sig`.
    ///
    /// The pair is the hazard: the `.sig` read was guarded first and this one was
    /// missed, and `shutdown` reaches the relaunch guard with no
    /// `binary_executes()` check in front of it. `std::fs::read` opens the path, and
    /// opening `/dev/watchdog` as root arms the hardware watchdog — no flag on the
    /// open undoes an open that already happened. An unprivileged user cannot make
    /// a device node but can make the symlink, on any install whose helper still
    /// lives in a directory they own.
    #[test]
    fn a_binary_symlinked_to_a_non_regular_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("veld-sig-binlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (k1, _) = gen_key(1);

        let fifo = dir.join("a-fifo");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a plain libc call with a valid NUL-terminated path.
        assert_eq!(unsafe { nix::libc::mkfifo(c.as_ptr(), 0o600) }, 0);

        // A real, readable 64-byte `.sig` so the sig guard passes and control
        // actually reaches the binary read — otherwise this test would pass for
        // the wrong reason.
        let binary = dir.join("veld-helper");
        std::os::unix::fs::symlink(&fifo, &binary).unwrap();
        std::fs::write(sig_path_for(&binary), slots(&[&k1], b"anything")).unwrap();
        assert!(
            read_detached_sig_slots(&binary).is_some(),
            "the .sig must be readable"
        );

        assert_eq!(classify_binary_signature(&binary), SigTrust::Untrusted);
        assert!(!verify_binary_signed(&binary));
        assert!(relaunch_guard(&binary).is_some());

        // ...and a binary reached **through a symlink to a real file** still
        // verifies, which is the half that keeps the guard from being a
        // regression. Asserted through `classify_binary_signature`, not through
        // `verify_data_slots`, because the guard being tested lives in the reader
        // and only the full path exercises it. An earlier version of this test
        // replaced the symlink with a plain file, which proved nothing about
        // symlinks at all.
        std::fs::remove_file(&binary).unwrap();
        let real = dir.join("the-real-helper");
        std::fs::write(&real, b"anything").unwrap();
        std::os::unix::fs::symlink(&real, &binary).unwrap();
        assert_eq!(
            classify_data(
                &[gen_key(1).1],
                &[],
                &std::fs::read(&binary).unwrap(),
                &read_detached_sig_slots(&binary).unwrap()
            ),
            SigTrust::Active,
            "a binary reached through a symlink to a regular file must still verify"
        );
        // And the real path, through the production entry point, with this
        // build's own keyring — which must refuse it, because the fixture is not
        // signed by the org.
        assert_eq!(classify_binary_signature(&binary), SigTrust::Untrusted);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The slot dedupe is counted, not assumed.
    ///
    /// Without this, the dedupe is invisible: it changes no verdict, so every
    /// other test passes with the loop reduced to a plain `any()` — and reducing
    /// it reopens the CPU amplification [`MAX_SIG_SLOTS`] documents, on a `.sig`
    /// the caller writes, with nothing red anywhere.
    #[test]
    fn each_distinct_slot_is_verified_at_most_once() {
        let mut sig = Vec::new();
        // Eight identical junk slots — the cheap version of the attack.
        for _ in 0..8 {
            sig.extend_from_slice(&[0xAAu8; SIG_SLOT_LEN]);
        }
        let mut calls = 0usize;
        assert!(!any_slot_verifies(&sig, |_| {
            calls += 1;
            false
        }));
        assert_eq!(calls, 1, "eight identical slots must cost one verification");

        // Eight distinct slots still cost eight: dedupe does not, and is not
        // claimed to, stop a deliberate attacker. See [`MAX_SIG_SLOTS`].
        let mut sig = Vec::new();
        for n in 0..8u8 {
            sig.extend_from_slice(&[n; SIG_SLOT_LEN]);
        }
        let mut calls = 0usize;
        assert!(!any_slot_verifies(&sig, |_| {
            calls += 1;
            false
        }));
        assert_eq!(calls, 8);

        // And it short-circuits: nothing after a match is verified.
        let mut calls = 0usize;
        assert!(any_slot_verifies(&sig, |_| {
            calls += 1;
            true
        }));
        assert_eq!(
            calls, 1,
            "verification must stop at the first slot that passes"
        );
    }

    /// A `.sig` that is a **symlink to a non-regular file** is refused, and the
    /// path is stat'd before it is opened.
    ///
    /// The hazard is not the FIFO used here — `O_NONBLOCK` already survives that.
    /// It is a device node: opening `/dev/watchdog` as root arms the hardware
    /// watchdog and dropping the descriptor reboots the machine, and no flag on
    /// the `open` can undo an `open` that already happened. An unprivileged user
    /// cannot create a device node but can create the **symlink**, either at a
    /// path they name on the install RPC or in a lib dir they own.
    ///
    /// The second half matters as much: a symlink to a *regular* file must still
    /// work, or this guard would break a legitimate layout to close a hostile one.
    #[test]
    fn a_sig_symlinked_to_a_non_regular_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("veld-sig-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (k1, p1) = gen_key(1);
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"payload").unwrap();

        let fifo = dir.join("a-fifo");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: a plain libc call with a valid NUL-terminated path.
        assert_eq!(unsafe { nix::libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        std::os::unix::fs::symlink(&fifo, sig_path_for(&binary)).unwrap();
        assert!(
            read_detached_sig_slots(&binary).is_none(),
            "a .sig symlinked to a FIFO must be refused"
        );
        assert!(!verify_binary_signed(&binary));

        // ...and a symlink to a real signature is still read.
        std::fs::remove_file(sig_path_for(&binary)).unwrap();
        let real = dir.join("elsewhere.bin");
        std::fs::write(&real, slots(&[&k1], b"payload")).unwrap();
        std::os::unix::fs::symlink(&real, sig_path_for(&binary)).unwrap();
        let read = read_detached_sig_slots(&binary).expect("a symlink to a regular file is fine");
        assert!(verify_data_slots(&[p1], b"payload", &read));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Slots past [`MAX_SIG_SLOTS`] are ignored, not fatal — and the read is
    /// bounded, so a huge `.sig` in a user-writable directory is not a memory DoS
    /// on every relaunch attempt.
    #[test]
    fn the_slot_read_is_bounded_and_the_excess_is_ignored() {
        let dir = std::env::temp_dir().join(format!("veld-sig-bound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (k1, p1) = gen_key(1);
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"payload").unwrap();

        // A real first slot, then a megabyte of junk.
        let mut sig = slots(&[&k1], b"payload");
        sig.extend_from_slice(&vec![0xEEu8; 1 << 20]);
        std::fs::write(sig_path_for(&binary), &sig).unwrap();

        let read = read_detached_sig_slots(&binary).unwrap();
        assert_eq!(read.len(), MAX_SIG_SLOTS * SIG_SLOT_LEN);
        assert!(verify_data_slots(&[p1], b"payload", &read));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_oversized_sig_is_rejected_without_slurping_it() {
        let dir =
            std::env::temp_dir().join(format!("veld-signing-oversize-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("veld-helper");
        std::fs::write(&binary, b"binary").unwrap();
        std::fs::write(sig_path_for(&binary), vec![0u8; 1 << 20]).unwrap();
        let _ = verify_binary_signed(&binary); // returns false; bounded read means it didn't OOM
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
