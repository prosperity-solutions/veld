//! RC1 characterization: Ctrl+C handling in [`run_command_streaming`].
//!
//! Lives in its own integration-test binary on purpose. The test raises
//! `SIGINT` at *this whole process*, which is what a real Ctrl+C on the
//! controlling terminal does — inside the unit-test binary it would also
//! interrupt any sibling test that happens to be streaming concurrently.

use std::collections::HashMap;

/// `run_command_streaming` reports the conventional `130` on interrupt, even
/// though the child runs in its own process group (`process_group(0)`) and so
/// never receives the terminal's SIGINT itself. veld catches the signal, kills
/// the child's group, and normalizes the code — this is what makes
/// `veld start --oneshot` exit deterministically under Ctrl+C regardless of how
/// the child handles signals.
#[tokio::test]
async fn streaming_interrupt_reports_130() {
    use tokio::signal::unix::{SignalKind, signal};

    // Register a SIGINT handler up front so the raise below is delivered to
    // tokio's signal registry rather than to the default disposition (which
    // would kill the test process outright). `ctrl_c()` inside
    // `run_command_streaming` shares that registry.
    let _armed = signal(SignalKind::interrupt()).expect("register SIGINT handler");

    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        nix::sys::signal::raise(nix::sys::signal::Signal::SIGINT).expect("raise SIGINT");
    });

    let out = veld_core::process::run_command_streaming(
        // Traps and ignores SIGINT/SIGTERM briefly, so a code of 130 can only
        // come from veld's own normalization, not from the child's exit status.
        &veld_core::config::CommandSpec::Shell("trap '' INT; sleep 30".to_owned()),
        &std::env::temp_dir(),
        &HashMap::new(),
        None,
        None,
    )
    .await
    .expect("interrupted run reports a code, not an error");

    assert_eq!(
        out.exit_code, 130,
        "an interrupted streaming run must report the conventional SIGINT code"
    );
}
