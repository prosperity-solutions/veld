//! One lock, shared by every unit test in this crate that touches process-wide
//! state.
//!
//! `std::env::set_var` is `unsafe` because it races any concurrent `var()` in
//! the same process, and cargo runs a crate's unit tests on parallel threads.
//! That was survivable while exactly one module read those variables — the
//! `instance` env test could truthfully say "no other test in the WHOLE test
//! binary reads or writes these".
//!
//! It stopped being true the moment `port::infrastructure_ports` started
//! calling `instance::daemon_port()`, because every `allocate` and
//! `reserve_fixed` in the port tests now reads `VELD_DAEMON_PORT` — on other
//! threads, under a different mutex, which is no exclusion at all.
//!
//! **One mutex, not two, and deliberately so.** Two locks would need a global
//! acquisition order to avoid deadlock, and nothing would enforce it. A single
//! lock also serialises the tests that bind real ports, which they needed
//! anyway. Take it **once** per test: it is not reentrant, and `let _ =`
//! instead of `let _guard =` drops it immediately and buys nothing.

use std::sync::{Mutex, MutexGuard};

static PROCESS_STATE_LOCK: Mutex<()> = Mutex::new(());

/// Serialise a test that reads or writes process-wide state — environment
/// variables, or a real TCP port.
///
/// Recovers from poisoning so one failing test does not cascade into every
/// other test that shares the lock.
pub(crate) fn process_state_guard() -> MutexGuard<'static, ()> {
    PROCESS_STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
