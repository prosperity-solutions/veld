use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use veld_core::setup::QuitOutcome;
use veld_core::state::RunStatus;

use crate::output;

/// How long the app gets to quit — whether it was asked over an Apple Event, sent
/// a `SIGTERM`, or told us its own pid and quit on its own.
///
/// Generous on purpose: `before-quit` persists window layout and hands back a
/// detached window's tabs, and a machine under load can take seconds over it.
/// Every path that runs out of this budget leaves the bundle alone rather than
/// forcing the issue.
const QUIT_TIMEOUT: Duration = Duration::from_secs(30);

/// What this run intends to do about Veld Desktop, decided *before* anything is
/// installed.
///
/// The ordering is the point. Working out whether the app is running — and
/// closing it, with the user's agreement — has to happen while nothing has moved
/// yet, so that a "no, leave it open" answer costs nothing and a failure here
/// aborts before the first byte is written.
#[derive(Debug, Default)]
struct DesktopPlan {
    /// Install/update the app as part of this run.
    update: bool,
    /// The bundle to replace, when the caller named one (the app passing its own).
    app_dir: Option<PathBuf>,
    /// Whether this run owes the user a reopened app, because it took one off
    /// screen. Set on every path that did, including the ones that then failed —
    /// an update that ends with no app is worse than one that ends with an old
    /// app.
    ///
    /// A flag rather than the path, and that is a fix rather than a
    /// simplification. The bundle is resolved *after* the update, because the
    /// path is not always known before it: an app running translocated has its
    /// `--app-path` dropped (see `bundle_dir_of`), so the installer places it in
    /// `/Applications` and the only correct answer to "what do I reopen" exists
    /// once that has happened. Holding a path decided beforehand meant reopening
    /// nothing at all in exactly that case — which is the case a `.dmg` download
    /// launches in.
    reopen: bool,
    /// Why the app half is not being attempted, when a caller that asked for it
    /// deserves to hear so.
    ///
    /// Only the handoff sets this, and only because of how the report is read:
    /// the app shows a dialog when `ok` is false and stays silent otherwise, so
    /// "the app was deliberately skipped" reported as success is an app that
    /// reopens on the old version having said nothing. A terminal run needs no
    /// such channel — the reason was printed where the user is looking.
    skipped: Option<String>,
}

/// The result of the update proper, in the shape the app's report needs.
struct Outcome {
    code: i32,
    /// The version this run was aiming at.
    version: String,
    /// What to tell the app went wrong, if anything did.
    error: Option<String>,
}

/// `veld update` -- update Veld to the latest version.
///
/// `wait_pid`, `relaunch` and `app_path` are the app handing its own update over.
/// It cannot update itself in place — an Electron app reads from its own bundle
/// while it runs — so it spawns this detached, quits, and lets the CLI move
/// *both* halves and reopen it. That is why this is `veld update` and not
/// `veld desktop update`: one release, one command, both halves, one restart.
pub async fn run(
    wait_pid: Option<u32>,
    relaunch: bool,
    app_path: Option<PathBuf>,
    target_version: Option<String>,
) -> i32 {
    // Pid 0 is dropped rather than waited on, and it is not a theoretical input:
    // `kill(0, signal)` addresses the *caller's own process group*, so
    // `wait_for_pid_exit(0, …)` never sees `ESRCH`, spins the whole 30s budget,
    // and then reports that Veld Desktop did not quit — about a process that
    // never existed. Treated as "no pid was given", which is what it means.
    let wait_pid = wait_pid.filter(|pid| *pid != 0);
    let handoff = wait_pid.is_some();
    let relaunch = honour_relaunch(relaunch, wait_pid);
    let app_dir = app_path.as_deref().and_then(super::desktop::bundle_dir_of);
    // The version the app was offered, when it named one. Reported back even on
    // the paths that install nothing, so the dialog the user eventually sees
    // names the release they clicked on rather than the one the CLI happens to be.
    let reported_version = target_version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    // Before anything is installed: the app must actually be gone. A pid that has
    // not exited is a process still reading the bundle we are about to replace,
    // and the honest response is to install nothing at all — the app is about to
    // reopen and would otherwise come back half-swapped.
    if let Some(pid) = wait_pid {
        output::print_info(&format!("Waiting for Veld Desktop (pid {pid}) to quit..."));
        if !veld_core::setup::wait_for_pid_exit(pid, QUIT_TIMEOUT).await {
            let msg = "Veld Desktop did not quit within 30s, so nothing was updated.";
            output::print_error(msg, false);
            veld_core::setup::write_desktop_update_report(
                &reported_version,
                Err(msg),
                veld_core::setup::UpdateHalf::Release,
            );
            // Reopen anyway when the app asked us to. "The pid did not exit" is
            // not the same as "the app is still on screen": `wait_for_pid_exit`
            // reads anything other than `ESRCH` as alive, so a pid this user may
            // not signal — or one recycled into another process — times out here
            // while the app itself is long gone. The app called `app.quit()`
            // before spawning this, so treating a timeout as "it must still be
            // running" is how the user ends up with no window, no update, and a
            // report only the app could have shown them. `open` on a running app
            // focuses it, so guessing wrong costs a Dock bounce.
            if relaunch {
                if let Some((app, _)) = veld_core::setup::desktop_app_status_in(app_dir.as_deref())
                {
                    veld_core::setup::open_desktop_app(&app);
                }
            }
            return 1;
        }
    }

    let plan = plan_desktop(app_dir, handoff, relaunch).await;
    let outcome = perform(&plan, target_version.as_deref()).await;

    // Written before the app is reopened, and that ordering is load-bearing for
    // the same reason it is in `veld desktop update`: the app reads this during
    // startup, so it has to be on disk before the launch it belongs to.
    if handoff {
        veld_core::setup::write_desktop_update_report(
            &outcome.version,
            outcome.error.as_deref().map_or(Ok(()), Err),
            veld_core::setup::UpdateHalf::Release,
        );
    }

    // Resolved now rather than before the update: see `DesktopPlan::reopen`. If
    // nothing is found there is nothing to open, which is the honest outcome of
    // an install that did not place a bundle.
    if plan.reopen {
        if let Some((app, _)) = veld_core::setup::desktop_app_status_in(plan.app_dir.as_deref()) {
            veld_core::setup::open_desktop_app(&app);
        }
    }

    outcome.code
}

/// Decide what happens to the app, and close it if it is in the way.
///
/// `handoff` means the app spawned this and has already quit itself — there is
/// nobody at a terminal to ask, and nothing to ask about.
async fn plan_desktop(app_dir: Option<PathBuf>, handoff: bool, relaunch: bool) -> DesktopPlan {
    // **First, before any early return.** `relaunch` means an app quit for this
    // and is owed a window back, and that debt does not depend on whether veld
    // then decides to update it. Setting it further down — after the platform
    // check, after the `VELD_DESKTOP=0` check — meant an app whose inherited
    // launchd environment happened to carry `VELD_DESKTOP=0` handed off, quit,
    // and was never reopened. Three review angles found that independently,
    // which is what a guard placed after its own preconditions looks like.
    let mut plan = DesktopPlan {
        app_dir,
        reopen: relaunch,
        ..Default::default()
    };

    // Quiet everywhere else: on Linux the AppImage updates itself and a .deb
    // belongs to the package manager, so there is no app here for veld to move.
    if std::env::consts::OS != "macos" {
        return plan;
    }

    // `VELD_DESKTOP=0` is documented as the opt-out for "a CI box or a server
    // that wants no Dock icon". Checked before anything else, so a machine that
    // asked for no app is never asked to close one either.
    if matches!(
        std::env::var("VELD_DESKTOP").as_deref(),
        Ok("0") | Ok("false") | Ok("no")
    ) {
        // Said out loud on the handoff path rather than reported as a success:
        // the user clicked a button offering them a new veld, and the app is
        // about to reopen on the version it had. `VELD_DESKTOP=0` is a real
        // answer, but it is not the answer they just gave.
        if handoff {
            plan.skipped = Some(
                "VELD_DESKTOP=0 is set in this machine's environment, so the app was not updated"
                    .to_string(),
            );
        }
        return plan;
    }

    let status = veld_core::setup::desktop_app_status_in(plan.app_dir.as_deref());

    // Nothing installed: the app half of this release is a fresh install, and
    // there is no window to close.
    let Some((path, _)) = &status else {
        plan.update = true;
        return plan;
    };

    let running = veld_core::setup::desktop_app_pids(path);
    if running.is_empty() {
        plan.update = true;
        return plan;
    }

    if handoff {
        // The pid we waited for is gone, yet something is still running from this
        // bundle — a second window's process, or an `--app-path` pointing at a
        // copy other than the one that handed off. Either way the bundle is in
        // use, so the CLI half proceeds and the app half does not.
        let reason = "another process is still running from Veld.app, so the app was left alone \
                      — the veld CLI was updated";
        output::print_error(reason, false);
        plan.skipped = Some(reason.to_string());
        return plan;
    }

    if !attended() {
        // An agent or a script is driving this. Closing someone's desktop app
        // without being asked is not a thing an unattended command may do, so
        // this reports the app half as skipped and moves on.
        output::print_info(
            "Veld Desktop is running and this is a non-interactive run, so the app was left \
             alone. Quit it and re-run `veld update`, or use the app's own 'Check for Updates…'.",
        );
        return plan;
    }

    if !ask_to_close_desktop() {
        output::print_info(
            "Leaving Veld Desktop alone. The CLI is still updated; the app catches up on the \
             next `veld update` or from its own 'Check for Updates…'.",
        );
        return plan;
    }

    output::print_info("Asking Veld Desktop to quit...");
    // Said before it disappears, because the gap is longer than it looks: the CLI
    // half runs first — a tarball, the service restarts, up to 45s each waiting
    // for the helper and the daemon to come back on the new binary — and only
    // then the app. Someone who reopens the app from the Dock in that window
    // trips the installer's running-app guard, and the run ends with the CLI
    // updated and the app not.
    output::print_info("It reopens when the update finishes — leave it closed until then.");
    match veld_core::setup::quit_desktop_app(path, QUIT_TIMEOUT).await {
        QuitOutcome::Quit | QuitOutcome::NotRunning => {
            plan.update = true;
            // We took it off screen, so putting it back is this run's job — on
            // every exit path, not only the successful one.
            plan.reopen = true;
        }
        QuitOutcome::Refused => {
            // It was asked *and signalled* before we gave up, so it may still be
            // on its way out and land just past the budget. Owe it a reopen: an
            // `open` on a running app focuses it, which is harmless, while not
            // reopening one that did quit leaves the user with nothing.
            plan.reopen = true;
            // Something on screen is unanswered. Never force it.
            output::print_error(
                "Veld Desktop did not quit, so the app was left alone — it may be showing a \
                 dialog. The veld CLI is still updated.",
                false,
            );
        }
    }
    plan
}

/// Whether there is a human at a terminal to answer a question.
///
/// Both halves matter, and `VELD_NON_INTERACTIVE` is read the way `install.sh`
/// reads it (`[ -n ... ]`): unset or empty stays interactive, any non-empty value
/// — including `0` — means non-interactive.
fn attended() -> bool {
    let forced_off = std::env::var("VELD_NON_INTERACTIVE")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal() && !forced_off
}

/// Ask permission to close the app, and say what it costs — which is close to
/// nothing, and the user has no way of knowing that.
///
/// Terminal sessions are the thing worth naming: they belong to the daemon, not
/// to the app window, and the daemon replays each session's scrollback when a
/// pane reattaches (`crates/veld-daemon/src/pty/holder.rs`). So a command still
/// running keeps running, and comes back with its output intact.
///
/// Defaults to yes on a bare Enter. The user typed `veld update`; closing an app
/// for a moment is within what that asked for, and the destructive-sounding half
/// of the question is the part that is not actually destructive.
fn ask_to_close_desktop() -> bool {
    use std::io::{BufRead, Write};

    println!();
    output::print_info("Veld Desktop is running, and its bundle cannot be replaced while it is.");
    println!(
        "  {}",
        output::dim(
            "Nothing is lost: terminal sessions belong to the daemon, keep running while the \
             app is closed, and reattach with their scrollback when it reopens."
        )
    );
    print!("  Close Veld Desktop, update both halves, and reopen it? [Y/n] ");
    let _ = std::io::stdout().flush();

    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    if read == 0 {
        // Newline of our own: `read_line` returning 0 means EOF, so the cursor is
        // still sitting after the prompt.
        println!();
    }
    consent(if read == 0 { None } else { Some(line.as_str()) })
}

/// What the typed answer means.
///
/// Split from the prompt because the prompt cannot be tested and this must be:
/// it is the gate in front of closing someone's running application.
///
/// A bare Enter is yes — the question was asked with `[Y/n]` and the user typed
/// `veld update`. **EOF is not**: `None` is nobody answering, which happens when
/// stdin is a closed pipe rather than the terminal `attended()` believed it to
/// be, and an answer nobody gave cannot be consent. Anything unrecognised is also
/// no, for the same reason — this branch closes an app, so it takes agreement
/// rather than the absence of refusal.
fn consent(line: Option<&str>) -> bool {
    let Some(line) = line else {
        return false;
    };
    matches!(line.trim().to_lowercase().as_str(), "" | "y" | "yes")
}

/// Whether `--relaunch` means anything on this invocation.
///
/// It is the app saying "I quit for this, put me back", so it means nothing
/// without the pid that says *which* app quit. Honouring it alone would make
/// `veld update --relaunch` **launch** a GUI app that was never running — and,
/// worse, launch it after the user answered "n" to the close prompt.
fn honour_relaunch(relaunch: bool, wait_pid: Option<u32>) -> bool {
    relaunch && wait_pid.is_some()
}

/// Which release to install, without asking GitHub twice.
///
/// `Ok(Some(v))` — install `v`. `Ok(None)` — already current. The caller's
/// remaining arms are unchanged, so a named target flows through exactly the
/// path a discovered one does.
async fn resolve_target(
    current: &str,
    target_version: Option<&str>,
) -> Result<Option<String>, anyhow::Error> {
    match target_version {
        // Named by the app, from the feed it was actually offered. No API call:
        // see the flag's own doc comment for the two ways asking again goes
        // wrong. Equality rather than `is_newer`, deliberately — a target older
        // than this binary is a downgrade the app asked for by name, and
        // refusing it here would leave the app looping on an offer nothing can
        // satisfy.
        Some(target) if target != current => Ok(Some(target.to_string())),
        Some(_) => Ok(None),
        None => veld_core::setup::check_update().await,
    }
}

/// The update itself, once the app question is settled.
async fn perform(plan: &DesktopPlan, target_version: Option<&str>) -> Outcome {
    let current = env!("CARGO_PKG_VERSION");
    output::print_info(&format!("Current version: {current}"));
    if target_version.is_none() {
        output::print_info("Checking for updates...");
    }

    match resolve_target(current, target_version).await {
        Ok(Some(new_version)) => {
            // Running environments are NOT stopped for an update. State lives in
            // a single SQLite DB with a forward-only migration system, so a
            // binary swap no longer risks the stale/incompatible state files
            // that the old JSON-per-run storage did. Service processes are
            // independent of the CLI/daemon/helper: the helper leaves Caddy
            // running across its own restart (URLs stay up) and the daemon GC
            // self-heals, so nothing needs tearing down. Environments keep
            // serving and pick up the new orchestrator on their next
            // `veld start`/`veld restart`.
            let running = find_running_environments();
            if !running.is_empty() {
                println!();
                output::print_info(&format!(
                    "{} environment(s) are running and will keep serving during the update:",
                    running.len()
                ));
                for (project, run_name) in &running {
                    println!(
                        "  {} {}",
                        output::cyan(run_name),
                        output::dim(&format!("({})", project.display()))
                    );
                }
                println!();
            }

            output::print_info(&format!("New version available: {current} → {new_version}"));

            // After install, privileged mode restarts the root helper via sudo
            // (see restart_services), with the helper's own binary-change
            // watcher + launchd/systemd KeepAlive as the no-sudo fallback. Both
            // recovery paths require the service to still be REGISTERED: a sudo
            // restart of a nonexistent job fails, and the watcher only helps a
            // job launchd already knows about. So a job that is entirely GONE is
            // the one case the update genuinely can't self-apply — it needs
            // `veld setup privileged` to re-register the LaunchDaemon. Check for
            // that BEFORE installing so it's reported as the pre-existing
            // problem it is, instead of a 45-second wait ending in a misleading
            // "did not pick up the new binary". In unprivileged mode the
            // installer bootstraps the LaunchAgent itself, so no pre-flight skip.
            let helper_dead_privileged = super::read_setup_mode().as_deref() == Some("privileged")
                && !privileged_helper_serviceable().await;
            if helper_dead_privileged {
                output::print_error(
                    "The veld-helper service is not registered with the service manager. The \
                     update will install new binaries, but the helper cannot restart itself — \
                     run `veld setup privileged` afterwards.",
                    false,
                );
            }

            output::print_info("Installing update...");

            match veld_core::setup::perform_update(&new_version).await {
                Ok(()) => {
                    output::print_success(&format!("Updated to {new_version}."));
                    cleanup_stale_binaries();
                    output::print_info("Restarting services with new binaries...");
                    restart_services(&new_version, helper_dead_privileged).await;
                    super::remove_legacy_hammerspoon().await;
                    // On this branch too, not only the "already latest" one. The
                    // CLI install runs with `VELD_DESKTOP=0`, so the app half is
                    // this call and nothing else — and the case the comment on
                    // `update_desktop_if_stale` names, an installer skipping a
                    // *running* app, is far likelier on a real version bump than
                    // on a no-op update.
                    let error = update_desktop_if_stale(&new_version, plan).await;
                    Outcome {
                        // The app half not landing does not fail the update: the
                        // CLI moved, the services restarted, and the app is the
                        // half a user can also fix by hand. It is *reported*
                        // rather than swallowed — through the return code's
                        // sibling, so the app's own report still names it.
                        code: 0,
                        version: new_version,
                        error,
                    }
                }
                Err(e) => {
                    output::print_error(&format!("Update failed: {e}"), false);
                    Outcome {
                        code: 1,
                        version: new_version,
                        error: Some(format!("the update failed: {e}")),
                    }
                }
            }
        }
        Ok(None) => {
            output::print_success(&format!("Already on the latest version ({current})."));
            // Also on the no-op branch, and not only for symmetry. `veld update`
            // runs the *old* binary, so the release that carries this cleanup is
            // never the release that runs it — without this arm the Spoon would
            // survive until the user's *next* actual version bump. Here, any
            // `veld update` after this one is installed clears it. Idempotent
            // and silent on a machine that never had the Spoon.
            super::remove_legacy_hammerspoon().await;
            // The app can lag the CLI even when the CLI is current: the installer
            // skips an app that is running, and someone may have installed the app
            // after the last update. Without this, `veld update` would report
            // success while leaving a stale app in /Applications and never mention
            // it — the CLI half moves, the app half silently does not.
            let error = update_desktop_if_stale(current, plan).await;
            Outcome {
                code: 0,
                version: current.to_string(),
                error,
            }
        }
        Err(e) => {
            output::print_error(&format!("Update check failed: {e}"), false);
            Outcome {
                code: 1,
                version: current.to_string(),
                error: Some(format!("the update check failed: {e}")),
            }
        }
    }
}

/// Bring Veld Desktop to `version`, installing it if this machine has none.
///
/// The app and the CLI are two halves of one release, so an update moves both —
/// the same reason the install script installs the app by default. This is the
/// *only* thing that moves the app half: `perform_update` runs the script with
/// `VELD_DESKTOP=0`, so both arms of `veld update` come through here and the app
/// is downloaded once, not twice. It also covers the case its own name is about —
/// an installer that skipped a *running* app.
///
/// macOS only, and quiet elsewhere: `desktop_app_status` reports nothing on Linux,
/// where the AppImage updates itself and a .deb belongs to the package manager.
///
/// The platform and `VELD_DESKTOP=0` checks are not repeated here: `plan_desktop`
/// makes both decisions before anything is installed, and `plan.update` is their
/// answer. Two places deciding the same thing is how the opt-out came to be
/// honoured on one path and not the other.
///
/// Returns the reason the app half did not land, for the app's own report. `None`
/// means it landed — or that this run was never going to touch it.
async fn update_desktop_if_stale(version: &str, plan: &DesktopPlan) -> Option<String> {
    if !plan.update {
        // `skipped` is `None` for every path that was never going to touch the
        // app — Linux, no handoff — and `Some` only where a caller asked for the
        // app half and is not getting it.
        return plan.skipped.clone();
    }

    let app_dir = plan.app_dir.as_deref();
    let existing = veld_core::setup::desktop_app_status_in(app_dir);
    if let Some((_, installed)) = &existing {
        if installed.as_deref() == Some(version) {
            return None;
        }
    }

    match &existing {
        Some((path, installed)) => output::print_info(&format!(
            "Veld Desktop at {} is {} — updating it to {version}.",
            path.display(),
            installed.as_deref().unwrap_or("an unknown version"),
        )),
        None => output::print_info(&format!("Installing Veld Desktop {version}...")),
    }
    let opts = veld_core::setup::DesktopInstall {
        // The bundle the caller named, so an app running from `~/Applications`
        // is the one replaced rather than a second copy appearing in
        // `/Applications`. `None` leaves the script its own search.
        app_dir: plan.app_dir.clone(),
        // Passed on so `install.sh`'s `cleanup` EXIT/INT/TERM trap reopens the
        // app too, which is a *second* net rather than a duplicate of the one in
        // `run`. The statement in `run` is an ordinary line of Rust: a Ctrl-C, a
        // SIGTERM or a panic during the ~113 MB download skips it, and on this
        // route the app has already been closed — so without this the interrupted
        // case ends with no window at all. `open` on a running app focuses it, so
        // both firing is harmless, which is the same argument `run` already makes.
        relaunch: plan.reopen,
        ..Default::default()
    };
    let result = veld_core::setup::install_desktop(version, &opts)
        .await
        .and_then(
            |()| match veld_core::setup::desktop_app_status_in(app_dir) {
                Some((_, v)) if v.as_deref() == Some(version) => Ok(()),
                // Same reason `veld desktop install` checks: the published install
                // script may predate the desktop section entirely and exit 0.
                _ => Err(anyhow::anyhow!(
                    "the install script ran but did not update the app"
                )),
            },
        );

    match result {
        Ok(()) => None,
        Err(e) => {
            // Not fatal: the CLI is fine, and the app is the half the user can
            // also fix by hand. Say so rather than failing an update that
            // succeeded.
            //
            // No "quit the app first" advice any more, and its absence is the
            // point: this path now runs only once the app is *already* closed,
            // so a failure here is a download, a checksum or a destination —
            // never the running-app skip that advice was written for.
            //
            // The script's own last complaint is lifted out of the log, the same
            // way `veld desktop update` does it (`desktop.rs`). Without this the
            // full route reports "install script exited with code 1" where the
            // route it replaces said "checksum verification failed" — the
            // information is on disk either way, because the app hands this
            // process an fd on that log, but nobody reads a log they were not
            // told about.
            let detail = veld_core::setup::desktop_update_log_path()
                .as_deref()
                .and_then(super::desktop::last_diagnostic);
            let e = match detail {
                Some(reason) => format!("{reason} ({e})"),
                None => format!("{e}"),
            };
            let reason = format!("Could not install Veld Desktop: {e}");
            output::print_error(
                &format!("{reason}. The veld CLI was updated; run `veld update` again to retry."),
                false,
            );
            Some(reason)
        }
    }
}

/// Find all running environments across all projects, for the informational
/// "these keep serving" notice. Returns (project_root, run_name) pairs.
fn find_running_environments() -> Vec<(std::path::PathBuf, String)> {
    let registry = match veld_core::db::Db::open().and_then(|db| db.registry()) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut running = Vec::new();
    for entry in registry.projects.values() {
        for (run_name, run_info) in &entry.runs {
            if run_info.status == RunStatus::Running {
                running.push((entry.project_root.clone(), run_name.clone()));
            }
        }
    }
    running
}

/// Remove stale daemon/helper copies next to the CLI binary.
///
/// If a dev previously ran `just dev-install` or manually copied binaries into
/// `~/.local/bin/`, those copies persist after `veld update` and can shadow the
/// real binaries in `~/.local/lib/veld/`. This cleans them up.
fn cleanup_stale_binaries() {
    let cli_dir = match std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_owned()))
    {
        Some(d) => d,
        None => return,
    };
    let lib = veld_core::paths::lib_dir();
    for name in ["veld-daemon", "veld-helper"] {
        let stale = cli_dir.join(name);
        let canonical = lib.join(name);
        if stale.exists() && stale != canonical && std::fs::remove_file(&stale).is_ok() {
            output::print_info(&format!("Removed stale {}", stale.display()));
        }
    }
}

/// Whether the privileged helper can pick up a new binary by itself: either
/// its socket answers (live helper with a watcher), or the service manager
/// still has it registered (launchd KeepAlive/WatchPaths or systemd
/// Restart=always relaunch it onto the new binary even if the process is
/// transiently down).
async fn privileged_helper_serviceable() -> bool {
    let socket = veld_core::helper::system_socket_path();
    let client = veld_core::helper::HelperClient::new(&socket);
    if client.status().await.is_ok() {
        return true;
    }
    if cfg!(target_os = "macos") {
        // Only a definitive "no job" counts as unserviceable — a failed/timed
        // out query (None) must not scare the user into re-running setup.
        veld_core::setup::launchd_job_registered("system", veld_core::setup::HELPER_LABEL_MACOS)
            .await
            != Some(false)
    } else {
        veld_core::setup::systemd_pid_query(veld_core::setup::HELPER_SERVICE_LINUX, false).await
            != Some(None)
    }
}

/// Restart the helper/daemon so they run the newly installed binaries, then
/// verify the helper actually came back healthy.
///
/// A managed helper (privileged/unprivileged) restarts *itself* when its binary
/// changes on disk (an in-process watcher exits so launchd relaunches the new
/// version — no sudo), complemented by the plist's `WatchPaths`. Rather than
/// assume that worked (the old bug), we poll until the helper reports the new
/// version, and give actionable guidance if it doesn't.
///
/// `target_version` is the version we just updated TO (from `check_update`),
/// NOT `env!("CARGO_PKG_VERSION")` — this process is the *old* CLI, so its
/// compile-time version is the version we updated *from*. Comparing against
/// that would invert the check (fail on every successful update, pass on a
/// failed one).
async fn restart_services(target_version: &str, helper_dead_privileged: bool) {
    let mode = super::read_setup_mode();

    // Auto mode has no persistent service: stop the ephemeral helper so the
    // next `veld start` re-bootstraps it with the new binary.
    if !matches!(mode.as_deref(), Some("privileged") | Some("unprivileged")) {
        output::print_info("Restarting auto-bootstrapped helper...");
        let user_socket = veld_core::helper::user_socket_path();
        let client = veld_core::helper::HelperClient::new(&user_socket);
        if client.shutdown().await.is_ok() {
            output::print_info("Helper stopped. It will restart on next `veld start`.");
        }
        return;
    }

    if helper_dead_privileged {
        // Already reported before the install; a dead privileged helper has no
        // watcher and nothing here can restart it without sudo, so waiting 45s
        // for its version to flip would only produce a second, misleading error.
        output::print_error(
            "Skipping helper restart check — the helper service was not registered before the \
             update. Run `veld setup privileged` to start it on the new version.",
            false,
        );
    } else {
        // In privileged mode the helper is a root service, so `veld update`
        // (unprivileged) cannot bounce it directly. Rather than passively wait
        // out the ~12s binary-watcher poll (which, if it slips past the 45s
        // budget, ends in a misleading "re-run veld setup privileged"),
        // deterministically restart it via sudo (a graceful SIGTERM that leaves
        // Caddy running — see restart_privileged_helper) — passwordless if a
        // credential is cached, otherwise a single interactive prompt. This is
        // the reliable path; the self-restart watcher stays as the no-sudo
        // fallback. Unprivileged mode's helper is a user LaunchAgent the
        // installer already bounced, so no sudo is needed there.
        if mode.as_deref() == Some("privileged") {
            output::print_info("Restarting veld-helper (privileged) with the new binary...");
            // A human is present only if we have a TTY AND weren't asked to run
            // non-interactively — otherwise sudo's password prompt would hang a
            // scripted/pty-driven update. Treat VELD_NON_INTERACTIVE as set only
            // when it's non-empty, matching install.sh's `[ -n ... ]` convention:
            // unset or empty (`=`) stays interactive; any non-empty value
            // (including `0`) means non-interactive, exactly as the shell reads
            // it. When interactive, warn before the prompt appears so an
            // unexpected sudo prompt from a dev tool isn't mistaken for malware.
            let non_interactive_env = std::env::var("VELD_NON_INTERACTIVE")
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let interactive = std::io::stdin().is_terminal() && !non_interactive_env;
            if interactive {
                output::print_info(
                    "veld may prompt for your sudo password to restart the privileged helper.",
                );
            }
            if !veld_core::setup::restart_privileged_helper(interactive).await {
                output::print_info(
                    "Could not restart the privileged helper via sudo — waiting for it to \
                     restart itself instead.",
                );
            }
        }

        // Verify against the specific socket for this mode — not `connect()` (which
        // falls through to the user socket and could latch onto a stale auto-helper
        // while the privileged one is mid-restart).
        let socket = if mode.as_deref() == Some("privileged") {
            veld_core::helper::system_socket_path()
        } else {
            veld_core::helper::user_socket_path()
        };
        output::print_info("Waiting for veld-helper to restart with the new binary...");
        if wait_for_helper_version(&socket, target_version, std::time::Duration::from_secs(45))
            .await
        {
            output::print_success("veld-helper restarted and healthy.");
        } else {
            output::print_error(
                "veld-helper did not pick up the new binary automatically. \
                 Run `veld doctor`; if it stays down, re-run `veld setup`.",
                false,
            );
        }
    }

    // The daemon is a user-level service (LaunchAgent / systemd --user) that the
    // installer restarts. Verify it came back on the new binary too — otherwise
    // `veld update` returns while the daemon is mid-restart, and an immediate
    // `veld doctor` shows "Daemon: not running / Feedback server not responding"
    // even though it self-heals moments later.
    output::print_info("Waiting for veld-daemon to restart with the new binary...");
    if wait_for_daemon_version(target_version, std::time::Duration::from_secs(45)).await {
        output::print_success("veld-daemon restarted and healthy.");
    } else {
        output::print_error(
            "veld-daemon did not pick up the new binary automatically. \
             Run `veld doctor`; if it stays down, re-run `veld setup`.",
            false,
        );
    }
}

/// Poll the daemon's `/api/health` until it reports `expected_version`, or the
/// timeout elapses.
///
/// The daemon is restarted by the installer — on macOS signalled to exit and
/// relaunched by launchd onto the new binary (`restart_launch_agent` in install.sh),
/// on Linux a plain `systemctl --user restart veld-daemon` — so
/// its HTTP endpoint goes down and comes back; waiting for the version to match
/// confirms the NEW daemon is serving, not a lingering old instance or a
/// pre-change daemon that has no `version` field (which reports nothing and
/// correctly times out into the actionable error).
async fn wait_for_daemon_version(expected_version: &str, timeout: std::time::Duration) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let start = std::time::Instant::now();
    loop {
        if let Ok(resp) = client
            .get(format!("{}/api/health", veld_core::instance::daemon_base()))
            .send()
            .await
        {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if body.get("version").and_then(|v| v.as_str()) == Some(expected_version) {
                    return true;
                }
            }
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Poll the helper on `socket` until it reports `expected_version`, or the
/// timeout elapses.
///
/// The managed helper keeps serving the OLD binary until its watcher fires
/// (~12s), so we wait for the version to actually flip rather than treating
/// "a helper is reachable" as success. A pre-change helper (no `version` field)
/// reports `None` and never matches, so this correctly times out into the
/// actionable error instead of falsely reporting success on the first update.
async fn wait_for_helper_version(
    socket: &std::path::Path,
    expected_version: &str,
    timeout: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    let client = veld_core::helper::HelperClient::new(socket);
    loop {
        if let Ok(Some(v)) = client.version().await {
            if v == expected_version {
                return true;
            }
        }
        if start.elapsed() >= timeout {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{DesktopPlan, consent, honour_relaunch, plan_desktop};

    /// `plan_desktop` reads the process environment, which is global.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn plan(relaunch: bool) -> DesktopPlan {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(plan_desktop(None, true, relaunch))
    }

    /// The invariant three review angles found broken independently: an app that
    /// quit for this update is owed a window back on **every** path out of
    /// `plan_desktop`, including the ones that decide not to touch it.
    ///
    /// `VELD_DESKTOP=0` is the early return that had the bug — an ambient value in
    /// the app's inherited launchd environment meant it quit and never came back.
    /// Adding any new early return above the struct literal reintroduces it, and
    /// this is what fails when someone does. The same call also pins that the skip
    /// is *reported* rather than passed off as success, since the app shows a
    /// dialog only when the report says the update failed.
    #[test]
    fn an_app_that_quit_for_this_is_reopened_even_when_it_is_not_updated() {
        // One guard for the whole test. Taking it twice would deadlock on the
        // second call — `let _guard = …` shadows the binding without dropping it,
        // and this mutex is not reentrant.
        let _guard = env_lock();
        // SAFETY: the lock above is held for the duration of this test.
        unsafe { std::env::set_var("VELD_DESKTOP", "0") };
        let opted_out = plan(true);
        // And the debt is only owed when an app actually quit for this.
        let no_handoff = plan(false);
        unsafe { std::env::remove_var("VELD_DESKTOP") };

        // Platform-independent, and deliberately asserted on every platform: the
        // debt is created by the struct literal, so this is what fails when a new
        // early return is added above it — on whichever runner gets there first.
        assert!(
            opted_out.reopen,
            "an app that handed off and quit must be reopened even when VELD_DESKTOP=0 \
             means veld will not update it",
        );
        assert!(!opted_out.update);
        assert!(!no_handoff.reopen);

        // macOS only, and the scope is the finding rather than a convenience:
        // off macOS `plan_desktop` returns at the platform check *above* the
        // `VELD_DESKTOP` branch, so there is no opt-out to report and `skipped`
        // is correctly `None`. Asserting it everywhere claimed an invariant the
        // code does not hold — which is how this test passed locally and failed
        // on CI's Linux runner.
        #[cfg(target_os = "macos")]
        assert!(
            opted_out.skipped.is_some(),
            "on macOS a skipped app half must reach the report, or the app reopens on \
             the old version having said nothing",
        );
        #[cfg(not(target_os = "macos"))]
        assert!(
            opted_out.skipped.is_none(),
            "off macOS veld never manages the app, so there is nothing to report as \
             skipped",
        );
    }

    #[test]
    fn a_named_target_is_installed_without_asking_github() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // Named and different → install it. No network call happens on this
        // arm, which is the point: the app already learned the release from
        // electron-updater's feed, and asking `api.github.com` a second time is
        // both rate-limited per IP and briefly out of step after a release.
        assert_eq!(
            rt.block_on(super::resolve_target("16.7.1", Some("16.8.0")))
                .unwrap(),
            Some("16.8.0".to_string()),
        );

        // Named and equal → nothing to install. The app half still runs, which
        // is how a lagging app catches up to a current CLI.
        assert_eq!(
            rt.block_on(super::resolve_target("16.8.0", Some("16.8.0")))
                .unwrap(),
            None,
        );

        // A target *older* than this binary is honoured rather than refused:
        // the app asked for it by name, and rejecting it here would leave the
        // app looping on an offer nothing can satisfy.
        assert_eq!(
            rt.block_on(super::resolve_target("16.8.0", Some("16.7.1")))
                .unwrap(),
            Some("16.7.1".to_string()),
        );
    }

    #[test]
    fn relaunch_without_a_pid_is_not_a_reason_to_launch_anything() {
        // `--relaunch` means "I quit for this, put me back". On its own it would
        // *start* a GUI app that was never running — including right after the
        // user answered "n" to the close prompt.
        assert!(honour_relaunch(true, Some(4321)));
        assert!(!honour_relaunch(true, None));
        assert!(!honour_relaunch(false, Some(4321)));
        assert!(!honour_relaunch(false, None));
    }

    #[test]
    fn a_bare_enter_agrees_and_anything_unrecognised_does_not() {
        for yes in ["", "\n", "y", "Y\n", "yes", "  YES  \n"] {
            assert!(consent(Some(yes)), "{yes:?} should agree");
        }
        for no in ["n", "N\n", "no", "later", "q", "?", "yeah"] {
            assert!(!consent(Some(no)), "{no:?} should not agree");
        }
    }

    #[test]
    fn nobody_answering_is_not_agreement() {
        // `None` is EOF: stdin was a closed pipe, not the terminal `attended()`
        // took it for. This gate closes a running application, so the absence of
        // an answer must read as "no" — the opposite default from the bare Enter
        // a human at a prompt gives.
        assert!(!consent(None));
    }
}
