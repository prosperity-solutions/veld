//! What the CLI actually hands `install.sh`.
//!
//! This contract had no test and could not have had one: `run_install_script`
//! fetches the *published* script, so every claim about the environment it
//! passes — "an app update never touches the CLI", "the app names the bundle to
//! replace", "an ambient variable cannot decide whether a GUI app gets
//! installed" — was only checkable by releasing and watching. `VELD_INSTALL_SCRIPT`
//! points the same code path at a local file; here that file is a recorder.
//!
//! Every test in this file mutates process-wide environment variables, so they
//! take `ENV_LOCK` rather than relying on `--test-threads=1`, which nothing in
//! this repo passes.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    // A poisoned lock means another test panicked mid-run; the environment is
    // still ours to reset, so carry on rather than cascade the failure.
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

struct Recorded {
    vars: Vec<(String, String)>,
}

impl Recorded {
    fn get(&self, key: &str) -> Option<&str> {
        self.vars
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

/// Run `f` against a stand-in `install.sh` and return the environment it saw.
fn record<F, Fut>(dir: &Path, ambient: &[(&str, &str)], f: F) -> (Recorded, bool)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), anyhow::Error>>,
{
    let _guard = env_lock();
    let out = dir.join("env.txt");
    // Removed first, so `expect("the recorder never ran")` below cannot be
    // satisfied by a *previous* call's file — two tests reuse one tempdir, and
    // a silently-not-run recorder would then be asserted against stale
    // environment and pass.
    let _ = std::fs::remove_file(&out);
    let script = dir.join("recorder.sh");
    // `env` rather than a hand-rolled loop: it reports what the process really
    // got, including variables this test did not think to name.
    std::fs::write(
        &script,
        "#!/usr/bin/env bash\nenv > \"$VELD_TEST_ENV_OUT\"\n",
    )
    .unwrap();

    // SAFETY: `ENV_LOCK` is held, so no other test in this binary is reading or
    // writing the environment concurrently.
    unsafe {
        std::env::set_var("VELD_INSTALL_SCRIPT", &script);
        std::env::set_var("VELD_TEST_ENV_OUT", &out);
        for (k, v) in ambient {
            std::env::set_var(k, v);
        }
    }

    let ok = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f())
        .is_ok();

    unsafe {
        std::env::remove_var("VELD_INSTALL_SCRIPT");
        std::env::remove_var("VELD_TEST_ENV_OUT");
        for (k, _) in ambient {
            std::env::remove_var(k);
        }
    }

    let text = std::fs::read_to_string(&out).expect("the recorder never ran");
    let vars = text
        .lines()
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect();
    (Recorded { vars }, ok)
}

#[test]
fn an_app_install_runs_the_script_in_app_only_mode() {
    let dir = tempfile::tempdir().unwrap();
    let opts = veld_core::setup::DesktopInstall {
        wait_pid: Some(4242),
        relaunch: true,
        app_dir: Some(PathBuf::from("/Users/x/Applications")),
        log: None,
    };
    let (env, ok) = record(dir.path(), &[], || {
        veld_core::setup::install_desktop("9.9.9", &opts)
    });

    assert!(ok);
    // The load-bearing one. Without it the script installs the CLI tarball,
    // bounces the daemon and may ask for a password — to update an app.
    assert_eq!(env.get("VELD_DESKTOP_ONLY"), Some("1"));
    assert_eq!(env.get("VELD_DESKTOP"), Some("1"));
    assert_eq!(env.get("VELD_VERSION"), Some("9.9.9"));
    assert_eq!(env.get("VELD_NON_INTERACTIVE"), Some("1"));
    assert_eq!(env.get("VELD_DESKTOP_WAIT_PID"), Some("4242"));
    assert_eq!(env.get("VELD_DESKTOP_RELAUNCH"), Some("1"));
    // The app naming its own bundle. Without it the script picks /Applications
    // and an app running from anywhere else gets a second copy written there.
    assert_eq!(env.get("VELD_DESKTOP_DIR"), Some("/Users/x/Applications"));
}

#[test]
fn a_default_app_install_asks_for_no_handoff_mechanics() {
    let dir = tempfile::tempdir().unwrap();
    // `veld desktop install` from a terminal: nothing to wait for, nothing to
    // reopen, and no bundle the caller has named.
    let plain = veld_core::setup::DesktopInstall::default();
    let (env, ok) = record(dir.path(), &[], || {
        veld_core::setup::install_desktop("9.9.9", &plain)
    });
    assert!(ok);
    assert_eq!(env.get("VELD_DESKTOP_ONLY"), Some("1"));
    assert!(!env.has("VELD_DESKTOP_WAIT_PID"));
    assert!(!env.has("VELD_DESKTOP_RELAUNCH"));
    assert!(!env.has("VELD_DESKTOP_DIR"));
}

#[test]
fn a_cli_update_never_installs_the_app_as_a_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    // `veld update`'s two halves are separate calls: this one moves the
    // binaries, `update_desktop_if_stale` moves the app. If this said anything
    // but 0, the app would be downloaded twice per update — and the script's
    // install-by-default would be deciding it, not the CLI.
    let (env, ok) = record(dir.path(), &[], || {
        veld_core::setup::perform_update("9.9.9")
    });
    assert!(ok);
    assert_eq!(env.get("VELD_DESKTOP"), Some("0"));
    assert!(!env.has("VELD_DESKTOP_ONLY"));
}

#[test]
fn an_ambient_handoff_variable_cannot_reach_the_script() {
    let dir = tempfile::tempdir().unwrap();
    // These three are pure mechanics — which pid to wait for, whether to reopen
    // the app, whether to skip the CLI half. A shell that exported one would
    // otherwise change what `veld update` does to this machine.
    let ambient = [
        ("VELD_DESKTOP_ONLY", "1"),
        ("VELD_DESKTOP_WAIT_PID", "1"),
        ("VELD_DESKTOP_RELAUNCH", "1"),
    ];
    let (env, ok) = record(dir.path(), &ambient, || {
        veld_core::setup::perform_update("9.9.9")
    });
    assert!(ok);
    assert!(!env.has("VELD_DESKTOP_ONLY"));
    assert!(!env.has("VELD_DESKTOP_WAIT_PID"));
    assert!(!env.has("VELD_DESKTOP_RELAUNCH"));
}

#[test]
fn an_ambient_desktop_dir_is_kept_but_an_explicit_one_wins() {
    let dir = tempfile::tempdir().unwrap();
    let ambient = [("VELD_DESKTOP_DIR", "/Volumes/Apps")];

    // Unlike the handoff knobs, this one is a real answer to a real question —
    // where this machine keeps its apps — so it survives.
    let plain = veld_core::setup::DesktopInstall::default();
    let (env, ok) = record(dir.path(), &ambient, || {
        veld_core::setup::install_desktop("9.9.9", &plain)
    });
    assert!(ok);
    assert_eq!(env.get("VELD_DESKTOP_DIR"), Some("/Volumes/Apps"));

    // …and the app naming its own bundle outranks it.
    let opts = veld_core::setup::DesktopInstall {
        app_dir: Some(PathBuf::from("/Users/x/Applications")),
        ..Default::default()
    };
    let (env, ok) = record(dir.path(), &ambient, || {
        veld_core::setup::install_desktop("9.9.9", &opts)
    });
    assert!(ok);
    assert_eq!(env.get("VELD_DESKTOP_DIR"), Some("/Users/x/Applications"));
}

#[test]
fn the_script_output_goes_to_the_log_when_one_is_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    // The app spawns the CLI detached with no streams, so without this every
    // word the installer says — including why it failed — is discarded. The
    // parent directory is created too: `~/.veld` may not exist yet on a machine
    // whose first veld action is opening the app.
    let log = dir.path().join("nested").join("desktop-update.log");
    let opts = veld_core::setup::DesktopInstall {
        log: Some(log.clone()),
        ..Default::default()
    };
    let script = dir.path().join("noisy.sh");
    std::fs::write(
        &script,
        "#!/usr/bin/env bash\nenv > \"$VELD_TEST_ENV_OUT\"\necho to-stdout\necho to-stderr >&2\n",
    )
    .unwrap();

    let _guard = env_lock();
    // SAFETY: `ENV_LOCK` is held for the duration of the run below.
    unsafe {
        std::env::set_var("VELD_INSTALL_SCRIPT", &script);
        std::env::set_var("VELD_TEST_ENV_OUT", dir.path().join("env.txt"));
    }
    let ok = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(veld_core::setup::install_desktop("9.9.9", &opts))
        .is_ok();
    unsafe {
        std::env::remove_var("VELD_INSTALL_SCRIPT");
        std::env::remove_var("VELD_TEST_ENV_OUT");
    }

    assert!(ok);
    let written = std::fs::read_to_string(&log).expect("no log written");
    assert!(written.contains("to-stdout"), "stdout missing: {written}");
    assert!(written.contains("to-stderr"), "stderr missing: {written}");
}

#[test]
fn a_failing_script_is_an_error_and_still_writes_its_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("desktop-update.log");
    let script = dir.path().join("failing.sh");
    std::fs::write(
        &script,
        "#!/usr/bin/env bash\necho the reason it failed >&2\nexit 7\n",
    )
    .unwrap();
    let opts = veld_core::setup::DesktopInstall {
        log: Some(log.clone()),
        ..Default::default()
    };

    let _guard = env_lock();
    // SAFETY: `ENV_LOCK` is held for the duration of the run below.
    unsafe { std::env::set_var("VELD_INSTALL_SCRIPT", &script) };
    let err = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(veld_core::setup::install_desktop("9.9.9", &opts))
        .unwrap_err();
    unsafe { std::env::remove_var("VELD_INSTALL_SCRIPT") };

    assert!(format!("{err:#}").contains('7'), "{err:#}");
    // The log is the only place the *reason* survives on the handoff path, so a
    // failure that discards it is the one failure this design cannot report.
    let written = std::fs::read_to_string(&log).unwrap();
    assert!(written.contains("the reason it failed"), "{written}");
}
