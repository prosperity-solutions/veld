//! Re-export the org signing gate for the helper's fail-closed relaunch paths.
//!
//! The org public key, the bounded `.sig` read, and the verification primitive
//! all live in [`veld_core::signing`] — so `veld doctor` checks a helper's
//! signature with the exact same key and code this gate uses, and a key
//! rotation has one place to land. The helper's `main.rs` (watcher +
//! `restart_blocker`) and `handler.rs` (`shutdown`) call [`relaunch_guard`]
//! before the helper will exit onto a changed on-disk binary.

pub(crate) use veld_core::signing::relaunch_guard;
