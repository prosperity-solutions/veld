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

#[cfg(test)]
mod tests {
    /// `.cargo/config.toml` blanks the instance variables for anything cargo
    /// runs, and this is the tripwire that says so out loud.
    ///
    /// Without it, a `cargo test` inherits whatever instance its terminal
    /// belongs to. That is not hypothetical: a terminal opened inside the dev
    /// stack's own `/ide` carries the `dev-daemon` node's `VELD_DB_PATH`
    /// (nothing calls `env_clear` between the daemon and a PTY holder), and
    /// `Db::path_override` consults it *before* the `veld-cargo.db` backstop
    /// whose entire job is stopping this. The tests then migrate the database a
    /// running dev daemon owns.
    ///
    /// Deliberately asserts the *environment*, not a resolved path: the
    /// backstop has its own test, and what is fragile here is the config file
    /// continuing to exist and continuing to say `force = true`.
    #[test]
    fn a_cargo_test_never_inherits_another_instances_identity() {
        let _guard = super::process_state_guard();
        for key in [
            "VELD_DB_PATH",
            "VELD_DAEMON_PORT",
            "VELD_DAEMON_SOCK",
            "VELD_PTY_DIR",
        ] {
            assert_eq!(
                std::env::var(key).unwrap_or_default(),
                "",
                "{key} reached a cargo test. Is `.cargo/config.toml` still \
                 present, and does it still set this with `force = true`? \
                 Without it, running the suite from a dev-stack terminal writes \
                 that instance's database."
            );
        }
    }
}
