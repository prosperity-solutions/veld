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
        verbose: false,
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
    // binaries, `run_desktop_step` moves the app. If this said anything
    // but 0, the app would be downloaded twice per update — and the script's
    // install-by-default would be deciding it, not the CLI.
    let (env, ok) = record(dir.path(), &[], || {
        veld_core::setup::perform_update("9.9.9", false)
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
        // Not a veld variable, and the reason it is here: `bash -c` sources
        // `$BASH_ENV` before the script body, so it decides what runs. The app
        // forwards its whole inherited environment into this call.
        ("BASH_ENV", "/tmp/veld-should-never-be-sourced.sh"),
        // `VELD_INSTALL_SCRIPT` is the other variable of this class and is also
        // stripped, but it cannot be asserted here: this harness *uses* it to
        // point at the recorder, so setting it as ambient would replace the
        // recorder rather than test anything.
    ];
    let (env, ok) = record(dir.path(), &ambient, || {
        veld_core::setup::perform_update("9.9.9", false)
    });
    assert!(ok);
    assert!(!env.has("VELD_DESKTOP_ONLY"));
    assert!(!env.has("VELD_DESKTOP_WAIT_PID"));
    assert!(!env.has("VELD_DESKTOP_RELAUNCH"));
    assert!(!env.has("BASH_ENV"));
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

/// The repo root, from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// The capability string is a **two-language contract with no compiler between
/// the halves**, so this is the compiler.
///
/// `veld desktop status --json` emits it from `crates/veld/src/commands/desktop.rs`
/// and Veld Desktop tests for it in `desktop/src/updatePolicy.js`. Rename or
/// mistype either side and nothing fails: the app silently drops to the app-only
/// route, `cargo test` and `npm test` both stay green, and the entire feature
/// disappears with no signal at all. That is precisely the failure a guard has to
/// be *constructed* to catch rather than commented about.
#[test]
fn every_advertised_capability_has_a_consumer_in_the_app() {
    let root = repo_root();
    let rust = std::fs::read_to_string(root.join("crates/veld/src/commands/desktop.rs")).unwrap();
    let js = std::fs::read_to_string(root.join("desktop/src/updatePolicy.js")).unwrap();

    // Every `caps.push("…")` in `capabilities()`, rather than one hardcoded
    // string. The difference matters for the *next* capability, not this one: a
    // test that names `full-update-handoff` protects only the capability that
    // already shipped, and the person adding the second one gets no signal at all.
    let advertised: Vec<&str> = rust
        .match_indices("caps.push(\"")
        .filter_map(|(i, m)| {
            let rest = &rust[i + m.len()..];
            rest.find('"').map(|end| &rest[..end])
        })
        .collect();

    assert!(
        !advertised.is_empty(),
        "no `caps.push(\"…\")` found in crates/veld/src/commands/desktop.rs — if \
         `capabilities()` was rewritten, rewrite this test with it rather than \
         deleting it: it is the only thing tying the two languages together",
    );

    for capability in advertised {
        assert!(
            js.contains(&format!("\"{capability}\"")),
            "the CLI advertises {capability:?} but desktop/src/updatePolicy.js never \
             mentions it. A capability with no consumer is dead weight the app will \
             never act on; add the constant there, or stop advertising it here.",
        );
    }

    // And the one the app keys its whole update route on, in the direction the
    // loop above cannot check: renamed in JS alone, the app silently falls back
    // to the app-only command with every suite green.
    assert!(
        js.contains(r#"FULL_UPDATE_HANDOFF = "full-update-handoff""#),
        "desktop/src/updatePolicy.js no longer looks for \"full-update-handoff\" — if \
         that is intended, update `capabilities()` in \
         crates/veld/src/commands/desktop.rs in the same commit",
    );
}

/// Lift one shell function out of `install.sh` so it can be run on its own.
///
/// The script is not sourceable — it installs veld — so the only way to test a
/// function in it is to cut it out and hand it to `bash`. Keyed on the definition
/// line and the first column-zero `}`, which is the shape every function in that
/// file has.
fn shell_function(name: &str) -> String {
    let script = std::fs::read_to_string(repo_root().join("install.sh")).unwrap();
    let header = format!("\n{name}() {{\n");
    let start = script
        .find(&header)
        .unwrap_or_else(|| panic!("install.sh no longer defines {name}()"))
        + 1;
    let body = &script[start..];
    let end = body
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{name}() in install.sh has no closing brace at column 0"))
        + 3;
    body[..end].to_string()
}

/// Run one of `install.sh`'s functions against a throwaway `HOME`.
fn run_shell_function(name: &str, args: &[&str], home: &Path) -> String {
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("set -u\n{}\n{name} \"$@\"", shell_function(name)))
        .arg("bash") // $0
        .args(args)
        .env("HOME", home)
        .output()
        .expect("bash");
    assert!(
        out.status.success(),
        "{name} exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The Veld Desktop preference is **one file read by two languages**, and nothing
/// else ties the halves together.
///
/// Rust writes `~/.veld/desktop.json` (`veld_core::desktop_pref`) and
/// `install.sh` reads it to decide whether to download a ~113 MB app; the script
/// writes it too, when it is the half that asked. Rename the key, nest it, or
/// change what "unset" looks like on either side and there is no error anywhere —
/// the app simply starts being installed for people who said no, or stops being
/// installed for people who said yes, with `cargo test` and `bash -n` both green.
/// That is the same class of failure as the capability string above, and it gets
/// the same kind of guard.
#[test]
fn the_desktop_preference_means_the_same_thing_in_both_languages() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    // Absent file: "nobody has been asked", which both halves must agree on —
    // this is the state every pre-existing user is in, and reading it as either
    // yes or no is a decision nobody made.
    assert_eq!(veld_core::desktop_pref::read_in(home), None);
    assert_eq!(run_shell_function("desktop_preference", &[], home), "");

    // Rust writes → the script reads.
    for (choice, expected) in [
        (veld_core::desktop_pref::DesktopChoice::Wanted, "yes"),
        (veld_core::desktop_pref::DesktopChoice::Unwanted, "no"),
    ] {
        veld_core::desktop_pref::write_in(home, choice).unwrap();
        assert_eq!(
            run_shell_function("desktop_preference", &[], home),
            expected,
            "install.sh cannot read what desktop_pref::write_in produced for {choice:?}",
        );
    }

    // The script writes → Rust reads. `install.sh` is the half that asks on a
    // fresh `curl … | bash`, where there is no veld binary yet to ask for it.
    for (arg, expected) in [
        ("yes", veld_core::desktop_pref::DesktopChoice::Wanted),
        ("no", veld_core::desktop_pref::DesktopChoice::Unwanted),
    ] {
        run_shell_function("record_desktop_preference", &[arg], home);
        assert_eq!(
            veld_core::desktop_pref::read_in(home),
            Some(expected),
            "desktop_pref cannot read what install.sh recorded for {arg:?}",
        );
    }

    // A torn or foreign file is "unset" on both sides, not a guess. The Rust half
    // pins this too; here it is the *script's* reader being held to it, since a
    // `case` pattern is easy to loosen into matching anything.
    let file = home.join(".veld").join("desktop.json");
    for junk in ["{\"wanted\":tr", "{}", "{\"wanted\":\"yes\"}", ""] {
        std::fs::write(&file, junk).unwrap();
        assert_eq!(
            run_shell_function("desktop_preference", &[], home),
            "",
            "install.sh read an answer out of {junk:?}",
        );
        assert_eq!(veld_core::desktop_pref::read_in(home), None, "{junk:?}");
    }

    // A later key does not change the answer on either side — this is what lets
    // the format grow a field without a flag day.
    std::fs::write(&file, "{\"wanted\":false,\"asked_by\":\"install.sh\"}").unwrap();
    assert_eq!(run_shell_function("desktop_preference", &[], home), "no");
    assert_eq!(
        veld_core::desktop_pref::read_in(home),
        Some(veld_core::desktop_pref::DesktopChoice::Unwanted)
    );

    // **The literal must not be readable out of another key's string value.** An
    // unanchored `*'"wanted":true'*` in the script read this as "yes" while serde
    // read the real field and answered "unwanted" — the two parsers disagreeing
    // about the file that gates a 113 MB download and, on one path, deleting an
    // application. The script's answer is now "unanswered", which is the only
    // direction a mismatch may fail in: it asks again.
    std::fs::write(&file, "{\"wanted\":false,\"note\":\"\\\"wanted\\\":true\"}").unwrap();
    assert_ne!(
        run_shell_function("desktop_preference", &[], home),
        "yes",
        "install.sh read `yes` out of a string value belonging to another key",
    );
    assert_eq!(
        veld_core::desktop_pref::read_in(home),
        Some(veld_core::desktop_pref::DesktopChoice::Unwanted)
    );
}

/// Both halves must name the same log file.
///
/// The CLI writes it (`desktop_update_log_path`) and the app both redirects the
/// handed-off process into it and offers to reveal it (`updateLogPath` in
/// `desktop/src/updater.js`). Two constants, two languages, one file — and a
/// comment in `updater.js` claims this test pins them, which it did not until it
/// existed.
#[test]
fn the_handoff_log_path_means_the_same_thing_in_both_languages() {
    let path = veld_core::setup::desktop_update_log_path().expect("a home directory");
    assert!(
        path.ends_with(".veld/desktop-update.log"),
        "{}",
        path.display()
    );

    let js = std::fs::read_to_string(repo_root().join("desktop/src/updater.js")).unwrap();
    assert!(
        js.contains(r#"path.join(os.homedir(), ".veld", "desktop-update.log")"#),
        "desktop/src/updater.js no longer resolves the same log path as \
         veld_core::setup::desktop_update_log_path",
    );
}

/// Every call into the install script is nested inside a command that is already
/// printing, so the script's own progress chatter, next-steps footer and success
/// banner are the caller's to print — not the script's.
///
/// Pinned because the failure is silent and cosmetic-looking: without it a single
/// `veld update` printed three "installed successfully!" banners, a first-install
/// footer halfway through an update, and two raw curl meters. It also decides
/// something that is *not* cosmetic — an embedded run leaves the privileged
/// helper restart to the CLI (see `install.sh`), so a regression here would have
/// two things racing to bounce a root service.
#[test]
fn every_scripted_install_runs_embedded() {
    let dir = tempfile::tempdir().unwrap();

    let (env, ok) = record(dir.path(), &[], || {
        veld_core::setup::perform_update("9.9.9", false)
    });
    assert!(ok);
    assert_eq!(env.get("VELD_EMBEDDED"), Some("1"));
    assert_eq!(env.get("VELD_VERBOSE"), Some(""));

    let opts = veld_core::setup::DesktopInstall::default();
    let (env, ok) = record(dir.path(), &[], || {
        veld_core::setup::install_desktop("9.9.9", &opts)
    });
    assert!(ok);
    assert_eq!(env.get("VELD_EMBEDDED"), Some("1"));
    assert_eq!(env.get("VELD_VERBOSE"), Some(""));
}

/// `--verbose` asks for the installer's output back — and **only** that.
///
/// `VELD_EMBEDDED` must stay `1`, because in `install.sh` it does not only mean
/// "be quiet": it is also what leaves the privileged helper restart to the CLI.
/// The first version of this change folded the two together, so `--verbose`
/// silently re-enabled the script's own `sudo launchctl kill` — a debug flag
/// bouncing a root service while the CLI restarted it too. This test is the thing
/// standing between that and a future edit that "simplifies" the two variables
/// back into one.
///
/// `VELD_VERBOSE` is empty rather than absent when off, deliberately: it is
/// inherited by anything the script re-executes, and an omitted one could be
/// filled in by an ambient value from the user's launchd session.
#[test]
fn verbose_asks_for_output_without_handing_back_the_privileged_restart() {
    let dir = tempfile::tempdir().unwrap();

    let (env, ok) = record(dir.path(), &[], || {
        veld_core::setup::perform_update("9.9.9", true)
    });
    assert!(ok);
    assert_eq!(env.get("VELD_EMBEDDED"), Some("1"));
    assert_eq!(env.get("VELD_VERBOSE"), Some("1"));

    let opts = veld_core::setup::DesktopInstall {
        verbose: true,
        ..Default::default()
    };
    let (env, ok) = record(dir.path(), &[], || {
        veld_core::setup::install_desktop("9.9.9", &opts)
    });
    assert!(ok);
    assert_eq!(env.get("VELD_EMBEDDED"), Some("1"));
    assert_eq!(env.get("VELD_VERBOSE"), Some("1"));
}

/// Neither variable can be decided by the user's shell.
///
/// The CLI sets both on every call, so this asserts the *override* rather than a
/// strip: an exported `VELD_VERBOSE=1` must not be able to make a normal update
/// print two success banners again, and an exported `VELD_EMBEDDED=""` must not
/// be able to hand the privileged helper restart back to the script.
#[test]
fn an_ambient_output_variable_cannot_decide_how_the_script_runs() {
    let dir = tempfile::tempdir().unwrap();

    let (env, ok) = record(
        dir.path(),
        &[("VELD_EMBEDDED", ""), ("VELD_VERBOSE", "1")],
        || veld_core::setup::perform_update("9.9.9", false),
    );
    assert!(ok);
    assert_eq!(env.get("VELD_EMBEDDED"), Some("1"));
    assert_eq!(env.get("VELD_VERBOSE"), Some(""));

    let (env, ok) = record(dir.path(), &[("VELD_VERBOSE", "")], || {
        veld_core::setup::perform_update("9.9.9", true)
    });
    assert!(ok);
    assert_eq!(env.get("VELD_VERBOSE"), Some("1"));
}
