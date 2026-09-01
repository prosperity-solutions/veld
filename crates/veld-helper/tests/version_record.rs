//! The embedded version record has to survive into the **built binary**, and
//! nothing else in the test suite can tell you whether it did.
//!
//! `VELD_HELPER_VERSION_RECORD` is data no code reads. It is kept alive by
//! `#[used]` and `#[unsafe(no_mangle)]` alone, and if a future edit drops either
//! — or a future toolchain stops honouring them — the helper compiles, passes
//! every unit test, runs perfectly, and ships **with no version inside it**. The
//! install RPC would then refuse it, so a release that lost this record is a
//! release that no privileged install can ever update onto: precisely the wedged
//! updater #338's rule 2 exists to prevent, discovered in production.
//!
//! `CARGO_BIN_EXE_veld-helper` is what makes this checkable: Cargo builds the
//! real binary for the integration test and hands over its path, so this asserts
//! against linked output rather than against source.

/// The record is in the binary, exactly once, and says what the crate says.
///
/// "Exactly once" is not pedantry. The scanner assembles its 16-byte needle at
/// runtime from two halves precisely so that a helper searching a *future*
/// helper does not also match its own copy of the needle — a second hit with a
/// different version makes `version_in_signed_bytes` return `None`, which reads
/// as "unversioned" and refuses the install. This is the test that would catch
/// somebody "simplifying" the split magic into an inline literal.
#[test]
fn a_built_helper_carries_exactly_one_version_record() {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_veld-helper"))
        .expect("cargo builds the helper for this test and hands over its path");

    assert_eq!(
        veld_core::signing::version_in_signed_bytes(&bytes).as_deref(),
        Some(env!("CARGO_PKG_VERSION")),
        "the built veld-helper does not carry its own version. Nothing reads \
         VELD_HELPER_VERSION_RECORD, so it survives only through #[used] and \
         #[unsafe(no_mangle)] — check those before anything else. A helper without this \
         record cannot be installed by any other helper."
    );
}

/// The scanner's own needle must not appear in a binary that contains a record,
/// beyond the record itself.
///
/// A helper is both scanner and scanned: it links `version_in_signed_bytes`, and
/// it is the thing a *newer* helper will scan. If the two 8-byte halves ever end
/// up contiguous outside the record — an inline literal, or a linker that placed
/// them next to each other — the second hit's version field is whatever follows
/// in rodata, and the install path stops working with nothing else failing.
#[test]
fn the_scanners_own_needle_is_not_a_second_record() {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_veld-helper")).unwrap();
    let record = veld_core::signing::version_record(env!("CARGO_PKG_VERSION"));
    let magic = &record[..16];

    let hits = bytes.windows(16).filter(|w| *w == magic).count();
    assert_eq!(
        hits, 1,
        "expected the 16-byte version magic exactly once in the built helper, found {hits}. \
         More than one means the scanner's needle is sitting in the binary contiguously — \
         see veld_core::signing::VERSION_MAGIC_A."
    );
}
