use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use veld_core::desktop_pref::{self, DesktopChoice};
use veld_core::setup::{DESKTOP_QUIT_TIMEOUT as QUIT_TIMEOUT, QuitOutcome};
use veld_core::state::RunStatus;
use veld_core::update_lock::{self, Acquired, Origin, Phase, UpdateGuard};

use crate::output;

/// How long `--console` waits for the window it opened to prove it exists.
///
/// Proof is the terminal's `veld update` taking the update lock — the one thing
/// only a process that actually started can do. `open` and every Linux terminal
/// emulator are fire-and-forget: a zero exit status means a launcher ran, not
/// that a window appeared, so without this handshake the app would hand the
/// update to a window that never opened and report success. Sized for a cold
/// Terminal.app launch on a loaded machine, which is seconds, not tens of them.
const CONSOLE_HANDSHAKE: Duration = Duration::from_secs(20);

/// The exit status a caller gets when another update already holds the lock.
///
/// `EX_TEMPFAIL`. "Try again shortly" is exactly what this is, and it has to be
/// distinguishable from the failures that mean something is actually wrong —
/// a coding agent driving `veld update` should retry this one and not the others.
const EX_TEMPFAIL: i32 = 75;

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
    console: bool,
    force: bool,
    verbose: bool,
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

    // **`sudo veld update` is refused, and the reason is not tidiness.** Under
    // sudo, `dirs::home_dir()` is *root's* (this repo already relies on that at
    // `setup.rs`'s `resolve_real_user_macos`), so the lock would be taken in
    // `/var/root/.veld` while every other reader — a plain `veld update`, the
    // command gate, `veld doctor`, the daemon monitor, Veld Desktop — looks in
    // the user's `~/.veld`. Single-flight against a lock nobody else can see is
    // no single-flight at all, and it is silent. The rest of `veld update`
    // already assumes it runs unprivileged and escalates only for the helper
    // restart, and `install.sh` does its own escalation for a `/usr/local`
    // install, so nothing legitimate needs this — a root-run install would also
    // leave root-owned binaries in the user's `~/.local`.
    if std::env::var("SUDO_USER").is_ok_and(|user| !user.is_empty()) {
        let msg = "`veld update` must not run under sudo — run it as yourself.";
        output::print_error(msg, false);
        println!(
            "  {}",
            output::dim(
                "It escalates on its own where it needs to (the privileged helper restart, \
                 and a /usr/local install inside install.sh), and prompts for your password \
                 when it does. Under sudo it would install root-owned files into your home \
                 and take its lock in root's."
            )
        );
        // **Every exit path a handoff can take owes the app a report**, and this
        // is a new one. The app has already quit and is waiting on
        // `desktop-update.json` to decide whether to say anything on relaunch —
        // an absent report reads as success, so a silent return here is an app
        // that reopens on the old version having been told nothing. Unlikely to
        // be reached (it needs `SUDO_USER` in the app's own launch environment)
        // and cheap to hold, which is the definition of an invariant worth
        // keeping rather than reasoning about.
        if handoff {
            veld_core::setup::write_desktop_update_report(
                &reported_version,
                Err(msg),
                veld_core::setup::UpdateHalf::Release,
            );
        }
        if relaunch {
            if let Some((app, _)) = veld_core::setup::desktop_app_status_in(app_dir.as_deref()) {
                veld_core::setup::open_desktop_app(&app);
            }
        }
        return 1;
    }

    // Refused before the lock is taken, so that the message names *this* run's
    // problem rather than a lock it just created. `--force` skips it the same way
    // it skips the acquisition below — the two must agree, or `--force` would
    // clear one gate and be stopped by the other.
    if let Some(state) = update_lock::current() {
        if !force {
            return refuse_busy(
                &state,
                handoff,
                &reported_version,
                relaunch,
                app_dir.as_deref(),
            );
        }
    }

    // A terminal window, when the app asked for one. Everything after this point
    // happens inside it instead of here — including taking the lock, which is how
    // this process knows the window is real.
    if console {
        match hand_over_to_console(
            wait_pid,
            relaunch,
            app_path.as_deref(),
            target_version.as_deref(),
            force,
            verbose,
        )
        .await
        {
            ConsoleHandoff::TookOver(launcher) => {
                output::print_success(&format!("The update is running in {launcher}."));
                return 0;
            }
            ConsoleHandoff::Failed(reason) => {
                output::print_info(&format!(
                    "Could not open a terminal window for the update ({reason}) — running it here \
                     instead."
                ));
            }
        }
    }

    let origin = match (console_child(), handoff) {
        (true, _) => Origin::Console,
        (false, true) => Origin::Desktop,
        (false, false) => Origin::Cli,
    };
    let mut guard = match update_lock::acquire(origin, target_version.clone(), force) {
        Ok(Acquired::Ours(guard)) => guard,
        Ok(Acquired::Busy(state)) => {
            return refuse_busy(
                &state,
                handoff,
                &reported_version,
                relaunch,
                app_dir.as_deref(),
            );
        }
        // A lock that cannot be taken must not stop an update: the failure mode
        // of proceeding is the one that existed before this lock did, and the
        // failure mode of refusing is a machine that can never update again
        // because `~/.veld` is briefly unwritable.
        Err(e) => {
            output::print_info(&format!(
                "Could not take the update lock ({e}) — continuing without it."
            ));
            return run_locked(
                None,
                wait_pid,
                relaunch,
                app_dir,
                &reported_version,
                target_version,
                verbose,
            )
            .await;
        }
    };
    guard.set_phase(Phase::WaitingForApp);
    run_locked(
        Some(guard),
        wait_pid,
        relaunch,
        app_dir,
        &reported_version,
        target_version,
        verbose,
    )
    .await
}

/// The update proper, with the lock already settled one way or the other.
///
/// `guard` is `None` only on the degraded path where the lock could not be
/// written at all; every phase call is therefore optional rather than the guard
/// being faked, so that "there is no lock" stays visible in the code instead of
/// being papered over by a no-op guard.
async fn run_locked(
    mut guard: Option<UpdateGuard>,
    wait_pid: Option<u32>,
    relaunch: bool,
    app_dir: Option<PathBuf>,
    reported_version: &str,
    target_version: Option<String>,
    verbose: bool,
) -> i32 {
    // Derived rather than passed: it is exactly `wait_pid.is_some()`, and a
    // parameter that restates another parameter is a parameter that can
    // disagree with it.
    let handoff = wait_pid.is_some();
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
                reported_version,
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

    let plan = plan_desktop(app_dir, handoff, relaunch, desktop_pref::read()).await;
    let outcome = perform(&plan, target_version.as_deref(), guard.as_mut(), verbose).await;

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

    // **The lock goes before the app comes back, and the order is not cosmetic.**
    // Veld Desktop quits itself on startup when an update is in progress, so
    // reopening while still holding would have this update close the very window
    // it exists to give back. Everything that mutates the machine is done by now:
    // what remains is `open`.
    if let Some(mut guard) = guard.take() {
        guard.set_phase(Phase::Finishing);
        guard.release();
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

/// What the *recorded preference* means for this run — the whole decision, with
/// no I/O in it.
///
/// Pure and separate because it is a four-way answer to three inputs and every
/// wrong cell is a user-visible bug: one of them downloads a 113 MB app onto a CI
/// box, another strands an IDE user on a stale app, another deletes an app nobody
/// asked to lose. A predicate written inline in `plan_desktop` could only be
/// tested by reading the maintainer's own `~/.veld/desktop.json` and whatever
/// happens to be in `/Applications`, i.e. by reporting on the machine it ran on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopGate {
    /// Install or update the app as this release requires.
    Proceed,
    /// The user said no. Leave the app half alone entirely.
    OptedOut,
    /// Nobody has answered, nobody can be asked, and there is no app here. Do not
    /// fetch one.
    SkipUnasked,
    /// Nobody has answered and this run may put the question.
    Ask,
}

/// The truth table, stated once.
///
/// The load-bearing asymmetry is in the unanswered row, and it is the difference
/// between two users veld cannot tell apart from a preference file: someone who
/// **has** the app is running the IDE and is entitled to have `veld update` keep
/// it in step, answer or no answer. Someone who does **not** have it has been
/// managing without it, so an update that cannot ask must not decide for them by
/// downloading it.
fn desktop_gate(recorded: Option<DesktopChoice>, installed: bool, may_ask: bool) -> DesktopGate {
    match recorded {
        Some(DesktopChoice::Wanted) => DesktopGate::Proceed,
        Some(DesktopChoice::Unwanted) => DesktopGate::OptedOut,
        None if may_ask => DesktopGate::Ask,
        None if installed => DesktopGate::Proceed,
        None => DesktopGate::SkipUnasked,
    }
}

/// Decide what happens to the app, and close it if it is in the way.
///
/// `handoff` means the app spawned this and has already quit itself — there is
/// nobody at a terminal to ask, and nothing to ask about.
/// `recorded` is the preference, passed in rather than read here — the only
/// injection point this function has. Without it every test of the branches below
/// would read the maintainer's own `~/.veld/desktop.json` and report on the
/// machine it ran on, which is why the one existing test could only ever exercise
/// the `VELD_DESKTOP=0` return above them.
async fn plan_desktop(
    app_dir: Option<PathBuf>,
    handoff: bool,
    relaunch: bool,
    recorded: Option<DesktopChoice>,
) -> DesktopPlan {
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

    // **Does this machine want the app at all?** Settled before anything about
    // *this* release is considered, because the answer decides whether the
    // ~113 MB download happens at all — which is the complaint this exists to
    // answer: a user who drives veld purely as an orchestrator was paying for the
    // IDE app on every single update.
    let path_of = |s: &Option<(PathBuf, Option<String>)>| s.as_ref().map(|(p, _)| p.clone());
    match desktop_gate(recorded, status.is_some(), !handoff && attended()) {
        // Answered yes, or answered nothing while already owning the app: today's
        // behaviour, unchanged, all the way down.
        DesktopGate::Proceed => {}
        DesktopGate::OptedOut => {
            // Deliberately **not** a removal. The recorded answer stops veld
            // fetching and updating the app; it does not license deleting a bundle
            // the user may have put there by hand since — a `.dmg` drag is a more
            // recent signal than an old "no", and silently deleting an app is not
            // a thing `veld update` may do. Named rather than silent, so the state
            // is discoverable from the run that acts on it.
            if let Some(path) = path_of(&status) {
                output::print_info(&format!(
                    "Veld Desktop is at {} but you opted out, so the app was left alone.",
                    path.display()
                ));
                println!(
                    "  {}",
                    output::dim(
                        "'veld desktop install' keeps it updated again; 'veld desktop uninstall' \
                         removes it."
                    )
                );
            }
            if handoff {
                // The app quit for this and is about to reopen on its old version.
                // Silence would read as success.
                plan.skipped = Some(
                    "you opted out of Veld Desktop, so the app was not updated — run \
                     `veld desktop install` to opt back in"
                        .to_string(),
                );
            }
            return plan;
        }
        DesktopGate::SkipUnasked => {
            // No app, no answer, nobody to ask. This is the case that used to hand
            // a CI box and an agent-driven update a GUI app nobody wanted.
            output::print_info(
                "Veld Desktop is not installed and this run cannot ask — skipping the app. \
                 Run `veld desktop install` to add it.",
            );
            return plan;
        }
        DesktopGate::Ask => match super::desktop::ask_desktop_choice(path_of(&status).as_deref()) {
            Some(DesktopChoice::Wanted) => {}
            Some(DesktopChoice::Unwanted) => {
                // The one place `veld update` removes the app, and it is
                // authorised by the answer that was *just* given rather than by a
                // stored one — which is the difference between reconciling the
                // machine and deleting somebody's app behind their back.
                //
                // Nothing to undo for `reopen`: this arm is only reachable with
                // `!handoff`, and `honour_relaunch` makes `relaunch` false without
                // a pid, so no app quit for this run and none is owed a window.
                if let Some(path) = path_of(&status) {
                    output::print_info(&format!("Removing {}...", path.display()));
                    match veld_core::setup::remove_desktop_app(&path).await {
                        Ok(()) => output::print_info("Removed. The CLI update continues."),
                        Err(e) => {
                            output::print_error(&format!("{e:#}"), false);
                            println!(
                                "  {}",
                                output::dim(
                                    "veld will not install it again either way — run \
                                     'veld desktop uninstall' once it has quit."
                                )
                            );
                        }
                    }
                }
                return plan;
            }
            // Nothing was recorded — EOF, or a keystroke that is not an answer.
            // The question stays open and this run takes the same *decision* as
            // the unasked case (keep an app that is here, fetch nothing that is
            // not) without repeating its message: telling somebody who was just
            // asked that there was nobody to ask is the wrong sentence.
            None if status.is_none() => return plan,
            None => {}
        },
    }

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

// ---------------------------------------------------------------------------
// Single-flight
// ---------------------------------------------------------------------------

/// Tell the caller somebody else is already updating, and put the app back if we
/// took it off screen.
///
/// The handoff path needs the report as much as any failure does — more, in fact:
/// it is the *only* channel back to an app that has already quit, and "another
/// update is running" reported as silence is precisely the invisible outcome this
/// whole change exists to remove.
fn refuse_busy(
    state: &veld_core::update_lock::UpdateState,
    handoff: bool,
    reported_version: &str,
    relaunch: bool,
    app_dir: Option<&std::path::Path>,
) -> i32 {
    let msg = state.describe(chrono::Utc::now());
    output::print_error(&msg, false);

    // **A window that lost the race owes the user nothing, and must not pretend
    // otherwise.** A console child only exists because an outer process launched
    // it, and that outer process is the one holding the report-and-relaunch duty:
    // either it refused here itself (and already wrote the report and reopened
    // the app), or its handshake timed out and it is *currently installing* while
    // holding this very lock. Writing a failure report from here would overwrite
    // that run's success — the app's next launch would announce a failed update
    // that actually worked — and reopening the app would put a window back over a
    // bundle swap in progress. The user is looking at this window; the message
    // above is the whole report they need.
    if console_child() {
        return EX_TEMPFAIL;
    }
    if let Some(tty) = &state.tty {
        output::print_info(&format!("It may be waiting for an answer in {tty}."));
    }
    println!(
        "  {}",
        output::dim(&format!(
            "Watch it with `veld update --status`. If you know it is dead, `veld update --force` \
             takes over; otherwise it is reclaimed automatically after {} minutes without \
             progress.",
            veld_core::update_lock::PHASE_TIMEOUT.as_secs() / 60
        ))
    );

    if handoff {
        veld_core::setup::write_desktop_update_report(
            reported_version,
            Err(&msg),
            veld_core::setup::UpdateHalf::Release,
        );
    }
    // The app quit for an update that is not going to happen here. Give it back —
    // the same debt `DesktopPlan::reopen` describes, owed on a path that never
    // gets as far as building a plan.
    if relaunch {
        if let Some((app, _)) = veld_core::setup::desktop_app_status_in(app_dir) {
            veld_core::setup::open_desktop_app(&app);
        }
    }
    EX_TEMPFAIL
}

/// The `--status --json` payload.
///
/// Split out because `skills/veld/SKILL.md` tells coding agents to read
/// `in_progress`, `phase` and friends — which makes these key names a contract,
/// and a contract with no test is a contract one rename silently breaks.
///
/// `in_progress` is deliberately **false for a stale lock** while the rest of the
/// fields still describe it: a consumer branching on one boolean gets the right
/// answer, and one that wants to explain the leftovers has `stale_reason`.
fn status_json(
    peeked: Option<&(
        veld_core::update_lock::UpdateState,
        Option<veld_core::update_lock::StaleReason>,
    )>,
    now: chrono::DateTime<chrono::Utc>,
) -> serde_json::Value {
    match peeked {
        Some((state, reason)) => serde_json::json!({
            "in_progress": reason.is_none(),
            "pid": state.pid,
            "origin": state.origin.as_str(),
            "version": state.version,
            "phase": state.phase.as_str(),
            "started_at": state.started_at.to_rfc3339(),
            "phase_at": state.phase_at.to_rfc3339(),
            "age_seconds": state.age(now).as_secs(),
            "tty": state.tty,
            "stale_reason": reason.map(|r| r.as_str()),
        }),
        None => serde_json::json!({ "in_progress": false }),
    }
}

/// `veld update --status` — what, if anything, is updating.
pub fn status(json: bool) -> i32 {
    let now = chrono::Utc::now();
    let peeked = veld_core::update_lock::peek();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status_json(peeked.as_ref(), now)).unwrap()
        );
        return 0;
    }

    match peeked {
        None => output::print_success("No update is running."),
        Some((state, None)) => {
            output::print_info(&state.describe(now));
            if let Some(tty) = &state.tty {
                println!("  {}", output::dim(&format!("Terminal: {tty}")));
            }
        }
        // Reported rather than hidden: a user who just watched an update die wants
        // to know the leftovers are harmless, and the next `veld update` clearing
        // them is a promise worth making out loud.
        Some((state, Some(reason))) => {
            output::print_success("No update is running.");
            println!(
                "  {}",
                output::dim(&format!(
                    "A lock left by pid {} is {} and will be cleared by the next update.",
                    state.pid,
                    match reason {
                        veld_core::update_lock::StaleReason::HolderGone => "gone",
                        veld_core::update_lock::StaleReason::Stalled => "stalled",
                        veld_core::update_lock::StaleReason::Unreadable => "unreadable",
                    }
                ))
            );
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Terminal handoff
// ---------------------------------------------------------------------------

/// Set inside the generated script so the run in the window knows what it is.
///
/// **Two paths branch on it**, so a stale inherited value is not cosmetic:
/// `refuse_busy` uses it to stay silent (no report, no relaunch) when a window
/// that lost the race is the one reporting, and `hand_over_to_console`'s
/// handshake only accepts a holder whose `Origin` is `Console`. A `veld update`
/// that wrongly believed itself a console child would therefore refuse a busy
/// lock without telling the app anything.
///
/// What bounds the inheritance is the launch path rather than a check: the app
/// is reopened with `/usr/bin/open`, and LaunchServices does not pass the
/// caller's environment to the app it launches — so the variable cannot leak
/// from an update into the Veld Desktop it reopens, which is the only loop that
/// could carry it.
const ORIGIN_ENV: &str = "VELD_UPDATE_ORIGIN";

fn console_child() -> bool {
    std::env::var(ORIGIN_ENV).as_deref() == Ok("console")
}

enum ConsoleHandoff {
    /// A window opened and its `veld update` took the lock.
    TookOver(String),
    /// No window; the caller should do the update itself.
    Failed(String),
}

/// The argv the window runs — the entire contract between this process and it.
///
/// Extracted from `hand_over_to_console` so it can be tested, because every arm
/// is load-bearing in a way that is invisible at the call site: **`--console` is
/// absent**, or the window opens a window and so on forever; `--wait-pid` is what
/// stops the window replacing a bundle the app still holds open; `--relaunch` is
/// the app's only way back onto the screen; `--target-version` is what keeps the
/// window off `api.github.com`, which is rate-limited per IP and briefly out of
/// step with the feed the app was offered.
fn console_args(
    wait_pid: Option<u32>,
    relaunch: bool,
    app_path: Option<&std::path::Path>,
    target_version: Option<&str>,
    force: bool,
    verbose: bool,
) -> Vec<String> {
    let mut args = vec!["update".to_string()];
    if let Some(v) = target_version {
        args.push("--target-version".into());
        args.push(v.to_string());
    }
    if let Some(pid) = wait_pid {
        args.push("--wait-pid".into());
        args.push(pid.to_string());
    }
    if relaunch {
        args.push("--relaunch".into());
    }
    if let Some(p) = app_path {
        args.push("--app-path".into());
        args.push(p.to_string_lossy().to_string());
    }
    if force {
        args.push("--force".into());
    }
    // Without this, `veld update --console --verbose` opens a window that runs
    // quietly — the one place the raw installer stream is hardest to get at any
    // other way, since the window is the whole point of the handoff.
    if verbose {
        args.push("--verbose".into());
    }
    args
}

/// Re-run this invocation inside a terminal window.
///
/// The arguments are reconstructed rather than forwarded verbatim because one of
/// them must not be: `--console` is dropped, or the window would open a window.
async fn hand_over_to_console(
    wait_pid: Option<u32>,
    relaunch: bool,
    app_path: Option<&std::path::Path>,
    target_version: Option<&str>,
    force: bool,
    verbose: bool,
) -> ConsoleHandoff {
    let Ok(exe) = std::env::current_exe() else {
        return ConsoleHandoff::Failed("this binary's own path is unknown".into());
    };

    let args = console_args(wait_pid, relaunch, app_path, target_version, force, verbose);

    // Captured before the launch, so a lock taken *after* this instant is the
    // only thing the handshake below will accept.
    let launched_at = chrono::Utc::now();

    let launcher = match veld_core::console::launch(
        &exe,
        &args,
        &[(ORIGIN_ENV, "console")],
        "Updating veld",
        "Update finished. You can close this window.",
    ) {
        Ok(launcher) => launcher,
        Err(e) => return ConsoleHandoff::Failed(e.to_string()),
    };

    // **The handshake, and the reason this is not fire-and-forget.** `open` and
    // every Linux terminal emulator report success for "a launcher started", not
    // for "a window appeared and ran your command" — so without waiting for
    // evidence, a machine with a broken `.command` association would have the app
    // quit, report that the update was running in a terminal, and update nothing.
    // Taking the update lock is evidence only the real thing can produce.
    //
    // **Three conditions, not one.** "A lock exists" is not evidence: this
    // process deliberately never acquires, so *any* holder would satisfy a bare
    // `pid != me` — including an unrelated `veld update` a user started in
    // another terminal a second ago. Accepting that would have the app quit,
    // announce "the update is running in your terminal", and exit 0 with no
    // window anywhere and nobody owing the app a relaunch. So the holder must
    // also say it *is* a console run, and must have started after the launch.
    let start = std::time::Instant::now();
    loop {
        if let Some(state) = update_lock::current() {
            if state.origin == update_lock::Origin::Console && state.started_at >= launched_at {
                return ConsoleHandoff::TookOver(launcher);
            }
        }
        if start.elapsed() >= CONSOLE_HANDSHAKE {
            return ConsoleHandoff::Failed(format!(
                "{launcher} did not start the update within {}s",
                CONSOLE_HANDSHAKE.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
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
async fn perform(
    plan: &DesktopPlan,
    target_version: Option<&str>,
    mut guard: Option<&mut UpdateGuard>,
    verbose: bool,
) -> Outcome {
    // Each of these is also the stall clock being reset. A phase that stops
    // moving for `PHASE_TIMEOUT` is what lets a *later* update reclaim a run
    // abandoned at a password prompt, so the granularity here is not cosmetic:
    // one phase for the whole run would mean the clock only ever ticks from the
    // start, and a slow-but-healthy install would look abandoned.
    macro_rules! phase {
        ($p:expr) => {
            if let Some(g) = guard.as_deref_mut() {
                g.set_phase($p);
            }
        };
    }

    let current = env!("CARGO_PKG_VERSION");
    phase!(Phase::Checking);
    if target_version.is_none() {
        // The current version is not announced separately: the header below
        // prints it beside the new one, where it means something, and on the
        // already-latest path `resolve_target`'s own line names it.
        output::print_info(&format!("Checking for updates ({current} installed)..."));
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

            if let Some(g) = guard.as_deref_mut() {
                g.set_version(&new_version);
            }

            // Decided once, up front, because the step counter and the step
            // itself must read the same answer — see `desktop_step`.
            let desktop = desktop_step(&new_version, plan);
            let steps = Steps::new(&desktop);

            println!();
            output::print_info(&format!(
                "{} {current} → {}",
                output::bold("veld"),
                output::bold(&new_version)
            ));
            println!();

            // After install, privileged mode restarts the root helper without
            // sudo (see restart_services): a `restart` request over the helper's
            // own socket, with its binary-change watcher + launchd/systemd
            // KeepAlive as the fallback, and sudo offered only if both fail.
            // Every one of those paths requires the service to still be
            // REGISTERED: the helper cannot be asked to restart if it is not
            // running, the watcher only helps a job launchd already knows about,
            // and a sudo restart of a nonexistent job fails. So a job that is
            // entirely GONE is
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

            steps.print(&format!("Installing veld {new_version}"));
            phase!(Phase::Installing);

            match veld_core::setup::perform_update(&new_version, verbose).await {
                Ok(()) => {
                    cleanup_stale_binaries();
                    steps.print("Restarting services");
                    phase!(Phase::RestartingServices);
                    let services_healthy =
                        restart_services(&new_version, helper_dead_privileged).await;
                    super::remove_legacy_hammerspoon().await;
                    phase!(Phase::UpdatingApp);
                    // On this branch too, not only the "already latest" one. The
                    // CLI install runs with `VELD_DESKTOP=0`, so the app half is
                    // this call and nothing else — and the case the comment on
                    // `run_desktop_step` names, an installer skipping a
                    // *running* app, is far likelier on a real version bump than
                    // on a no-op update.
                    let error = run_desktop_step(&new_version, &desktop, &steps, verbose).await;
                    // Last, and that ordering is the point: the old code printed
                    // "Updated to X" before the service restarts and the ~113 MB
                    // app download, so the command announced it was finished and
                    // then ran for another ten seconds.
                    //
                    // A green tick only when the run earned one. Moving the banner
                    // to the end put it *underneath* any "veld-helper did not pick
                    // up the new binary" printed by the restart step — so on
                    // exactly the machines this change is about, a half-applied
                    // update would have closed with an unqualified success.
                    println!();
                    if services_healthy {
                        output::print_success(&format!("Updated to {new_version}."));
                    } else {
                        output::print_info(&format!(
                            "Binaries updated to {new_version}, but a service above did not come \
                             back on it. Run `veld doctor`."
                        ));
                    }
                    print_install_summary(plan);
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
                    // Not printed when the script never ran: there is no
                    // "installer's own output" to go and look at, and the error
                    // above is already the whole story. This is the case a GitHub
                    // incident produces, so it is the one most people meet.
                    if !verbose && !veld_core::setup::is_install_script_unavailable(&e) {
                        println!(
                            "  {}",
                            output::dim(
                                "Re-run with `veld update --verbose` to see the installer's \
                                 own output."
                            )
                        );
                    }
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
            phase!(Phase::UpdatingApp);
            // The app is the only thing moving on this branch, so it is step 1
            // of 1 — `Steps` counts what this run will actually do rather than
            // what the full path would.
            let desktop = desktop_step(current, plan);
            let steps = Steps::app_only(&desktop);
            let error = run_desktop_step(current, &desktop, &steps, verbose).await;
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

/// The numbered-step printer for one update run.
///
/// The denominator is computed before the first step rather than hard-coded,
/// because the app step is conditional: a Linux box, `VELD_DESKTOP=0`, or an app
/// already on the new version all make it two steps rather than three. A fixed
/// `[n/3]` would have been a small, constant lie.
struct Steps {
    total: usize,
    done: std::cell::Cell<usize>,
}

impl Steps {
    fn new(desktop: &DesktopStep) -> Self {
        // Install and restart always happen; the app step is the variable one.
        Self::with_total(2 + usize::from(!matches!(desktop, DesktopStep::Skip(_))))
    }

    /// The already-latest branch, where the app is the only half that can move.
    fn app_only(desktop: &DesktopStep) -> Self {
        Self::with_total(usize::from(!matches!(desktop, DesktopStep::Skip(_))))
    }

    fn with_total(total: usize) -> Self {
        Self {
            total,
            done: std::cell::Cell::new(0),
        }
    }

    fn print(&self, label: &str) {
        let n = self.done.get() + 1;
        self.done.set(n);
        println!(
            "  {} {label}",
            output::dim(&format!("[{n}/{}]", self.total))
        );
    }

    /// Indent a detail line under the step it belongs to.
    fn detail(msg: &str) {
        println!("        {msg}");
    }

    /// A service that came back on the expected version. Named rather than
    /// ticked alone: "veld-helper 16.13.0" is the fact a user might want to
    /// check, where "restarted and healthy" only asserts it.
    fn service_ok(name: &str, version: &str) {
        Self::detail(&format!(
            "{} {name:<12} {}",
            output::checkmark(),
            output::dim(version)
        ));
    }
}

/// What the app half of this update will do, decided once and read twice.
///
/// Split out because the step counter and the step itself must agree, and the
/// alternative is two predicates that stay in step until one of them is edited —
/// which is exactly how the `VELD_DESKTOP=0` opt-out came to be honoured on one
/// path and not the other.
enum DesktopStep {
    /// Nothing to do. Carries the reason, for the app's own report: `None` for
    /// every path that was never going to touch the app (Linux, no handoff, an
    /// app already current), `Some` only where a caller asked for the app half
    /// and is not getting it.
    Skip(Option<String>),
    /// No bundle on this machine yet.
    Install {
        app_dir: Option<PathBuf>,
        reopen: bool,
    },
    /// Replace the bundle at `path`, currently on `installed`.
    Update {
        app_dir: Option<PathBuf>,
        reopen: bool,
        path: PathBuf,
        installed: Option<String>,
    },
}

impl DesktopStep {
    fn reopen(&self) -> bool {
        match self {
            DesktopStep::Skip(_) => false,
            DesktopStep::Install { reopen, .. } | DesktopStep::Update { reopen, .. } => *reopen,
        }
    }
}

/// Decide what the app half will do, without doing any of it.
///
/// The platform and `VELD_DESKTOP=0` checks are not repeated here: `plan_desktop`
/// makes both decisions before anything is installed, and `plan.update` is their
/// answer.
fn desktop_step(version: &str, plan: &DesktopPlan) -> DesktopStep {
    if !plan.update {
        return DesktopStep::Skip(plan.skipped.clone());
    }
    match veld_core::setup::desktop_app_status_in(plan.app_dir.as_deref()) {
        Some((_, installed)) if installed.as_deref() == Some(version) => DesktopStep::Skip(None),
        Some((path, installed)) => DesktopStep::Update {
            app_dir: plan.app_dir.clone(),
            reopen: plan.reopen,
            path,
            installed,
        },
        None => DesktopStep::Install {
            app_dir: plan.app_dir.clone(),
            reopen: plan.reopen,
        },
    }
}

/// Where the update left everything, printed once at the very end.
///
/// This is the summary `install.sh` used to print — from the middle of the run,
/// before the service restarts and the app half had happened. Reading the paths
/// back from `veld_core::paths` rather than from the script's stdout also means
/// it says where things *are*, not where a script said it was putting them.
fn print_install_summary(plan: &DesktopPlan) {
    let lib = veld_core::paths::lib_dir();
    println!();
    // Only rows that are actually there. `lib_dir()` prefers `~/.local/lib/veld`
    // whenever that directory merely *exists*, while the installer writes
    // `/usr/local/lib/veld` for a `/usr/*` install — so on a machine carrying
    // both, printing unconditionally would name three binaries this update never
    // touched. The banner this replaced printed the script's real `$LIB_DIR`;
    // stat'ing is how that stays true without threading the path back.
    let row = |label: &str, path: &std::path::Path| {
        if path.exists() {
            println!("  {label:<14} {}", output::dim(&path.display().to_string()));
        }
    };
    if let Ok(exe) = std::env::current_exe() {
        row("veld", &exe);
    }
    for name in ["veld-helper", "veld-daemon", "caddy"] {
        row(name, &lib.join(name));
    }
    // The caller's bundle, not a fresh guess: `desktop_app_status_in(None)`
    // searches `/Applications` first, so on a machine with a second copy in
    // `~/Applications` the summary would name the bundle this run did *not*
    // touch. Every other app decision in the run reads `plan.app_dir`.
    if let Some((path, _)) = veld_core::setup::desktop_app_status_in(plan.app_dir.as_deref()) {
        row("Veld Desktop", &path);
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
async fn run_desktop_step(
    version: &str,
    step: &DesktopStep,
    steps: &Steps,
    verbose: bool,
) -> Option<String> {
    let app_dir = match step {
        // `skipped` is `None` for every path that was never going to touch the
        // app — Linux, no handoff — and `Some` only where a caller asked for the
        // app half and is not getting it.
        DesktopStep::Skip(reason) => return reason.clone(),
        DesktopStep::Install { app_dir, .. } => {
            steps.print(&format!("Installing Veld Desktop {version}"));
            app_dir.clone()
        }
        DesktopStep::Update {
            app_dir,
            installed,
            path,
            ..
        } => {
            steps.print(&format!(
                "Updating Veld Desktop {} → {version}",
                installed.as_deref().unwrap_or("(unknown version)")
            ));
            println!("        {}", output::dim(&path.display().to_string()));
            app_dir.clone()
        }
    };
    let app_dir = app_dir.as_deref();

    let opts = veld_core::setup::DesktopInstall {
        // The bundle the caller named, so an app running from `~/Applications`
        // is the one replaced rather than a second copy appearing in
        // `/Applications`. `None` leaves the script its own search.
        app_dir: app_dir.map(std::path::Path::to_path_buf),
        // Passed on so `install.sh`'s `cleanup` EXIT/INT/TERM trap reopens the
        // app too, which is a *second* net rather than a duplicate of the one in
        // `run`. The statement in `run` is an ordinary line of Rust: a Ctrl-C, a
        // SIGTERM or a panic during the ~113 MB download skips it, and on this
        // route the app has already been closed — so without this the interrupted
        // case ends with no window at all. `open` on a running app focuses it, so
        // both firing is harmless, which is the same argument `run` already makes.
        relaunch: step.reopen(),
        verbose,
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
            //
            // Not when the script never ran, though: the download precedes the log
            // truncation, so on that path the "last complaint" is a leftover from an
            // earlier run and would attach a stale, wrong reason to a message that is
            // already complete. Same guard as `veld desktop update`.
            let detail = if veld_core::setup::is_install_script_unavailable(&e) {
                None
            } else {
                veld_core::setup::desktop_update_log_path()
                    .as_deref()
                    .and_then(super::desktop::last_diagnostic)
            };
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
/// Three tiers, none of which needs a password on the happy path. In privileged
/// mode the CLI first *asks* the helper to restart, over the socket it already
/// exposes to unprivileged callers ([`request_helper_restart`]) — the tier that
/// matters most, because the CLI knows the installer has finished writing where
/// the next tier can only guess. A helper too old to understand the request falls
/// back to its own binary-change watcher, which exits so launchd/systemd
/// relaunches it (~12s). Sudo is tier three and is only *offered*, after the
/// first two have had their full budget ([`offer_sudo_helper_restart`]).
///
/// None of that is assumed to have worked (the old bug): we poll until the helper
/// reports the new version, give actionable guidance if it doesn't, and return
/// whether every service came back — the caller's success banner depends on it.
///
/// Unprivileged mode's helper is a user LaunchAgent the installer already
/// bounced, so only the polling half applies there.
///
/// `target_version` is the version we just updated TO (from `check_update`),
/// NOT `env!("CARGO_PKG_VERSION")` — this process is the *old* CLI, so its
/// compile-time version is the version we updated *from*. Comparing against
/// that would invert the check (fail on every successful update, pass on a
/// failed one).
#[must_use]
async fn restart_services(target_version: &str, helper_dead_privileged: bool) -> bool {
    let mode = super::read_setup_mode();

    // Auto mode has no persistent service: stop the ephemeral helper so the
    // next `veld start` re-bootstraps it with the new binary.
    if !matches!(mode.as_deref(), Some("privileged") | Some("unprivileged")) {
        let user_socket = veld_core::helper::user_socket_path();
        let client = veld_core::helper::HelperClient::new(&user_socket);
        if client.shutdown().await.is_ok() {
            Steps::detail(&output::dim(
                "veld-helper stopped — it restarts on the next `veld start`",
            ));
        }
        return true;
    }

    let mut all_healthy = true;

    if helper_dead_privileged {
        all_healthy = false;
        // Already reported before the install; a dead privileged helper has no
        // watcher and nothing here can restart it without sudo, so waiting 45s
        // for its version to flip would only produce a second, misleading error.
        output::print_error(
            "Skipping helper restart check — the helper service was not registered before the \
             update. Run `veld setup privileged` to start it on the new version.",
            false,
        );
    } else {
        // Verify against the specific socket for this mode — not `connect()` (which
        // falls through to the user socket and could latch onto a stale auto-helper
        // while the privileged one is mid-restart).
        let privileged = mode.as_deref() == Some("privileged");
        let socket = if privileged {
            veld_core::helper::system_socket_path()
        } else {
            veld_core::helper::user_socket_path()
        };

        // In privileged mode the helper is a root service, so this unprivileged
        // process cannot bounce it directly. It does not need to: the helper
        // restarts *itself* on request, over the socket it already exposes to
        // the unprivileged CLI. Unprivileged mode's helper is a user LaunchAgent
        // the installer already bounced.
        if privileged {
            request_helper_restart(&socket, target_version).await;
        }

        // Say what is being waited for. The no-sudo paths are *slower* than the
        // sudo bounce they replaced — up to 12s for the watcher, and the whole
        // 45s budget on the bad day — and the step line alone left that as
        // unexplained silence, which reads as a hang.
        Steps::detail(&output::dim("waiting for veld-helper..."));

        // `||` short-circuits, so the sudo offer is only ever reached after the
        // no-sudo paths have had their full budget and failed.
        let healthy = wait_for_helper_version(&socket, target_version, HELPER_RESTART_TIMEOUT)
            .await
            || (privileged && offer_sudo_helper_restart(&socket, target_version).await);
        if healthy {
            Steps::service_ok("veld-helper", target_version);
        } else {
            all_healthy = false;
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
    Steps::detail(&output::dim("waiting for veld-daemon..."));
    if wait_for_daemon_version(target_version, std::time::Duration::from_secs(45)).await {
        Steps::service_ok("veld-daemon", target_version);
    } else {
        all_healthy = false;
        output::print_error(
            "veld-daemon did not pick up the new binary automatically. \
             Run `veld doctor`; if it stays down, re-run `veld setup`.",
            false,
        );
    }

    all_healthy
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
/// How long to wait for the helper to come back on the new binary before
/// offering sudo.
///
/// Sized against the slowest no-sudo path rather than picked round. The `restart`
/// ping normally lands in well under a second, but both it and the binary watcher
/// run the same safety gate, whose exec check can burn a full
/// `BINARY_EXEC_CHECK_TIMEOUT` (6s in `veld-helper`) before giving up — so a
/// watcher tick that has to wait one out costs 10s poll + 2s settle + 6s and
/// *fails*, and the retry needs another. 45s covers two such ticks with room; a
/// binary that fails the exec check twice over is the case the sudo offer exists
/// for, and by then the offer knows to say the binary is broken rather than
/// prompt.
const HELPER_RESTART_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// How long to wait after a sudo-driven restart. Short: sudo bounced the service
/// directly, so if it were going to come back it already has.
const SUDO_RESTART_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Ask the privileged helper to restart onto the new binary, without sudo.
///
/// Best-effort by design — every failure here falls through to the helper's own
/// binary watcher, which reaches the same end state within ~12s. The ping is
/// worth making anyway for a reason that is not speed: the CLI *knows* the
/// installer has finished writing, where the watcher can only infer it from a
/// settling size/mtime. That inference has lost the race in the field and left
/// launchd crash-looping on a half-written binary with no helper running at all.
///
/// Silent on every path. A user who did not ask how the helper restarts should
/// not have to read about it — the next line they see is either "restarted and
/// healthy" or a real problem.
async fn request_helper_restart(socket: &std::path::Path, target_version: &str) {
    let client = veld_core::helper::HelperClient::new(socket);

    // The watcher may have got there first: the installer wrote the binary
    // before this code runs, so on a slow enough install the helper is already
    // current. Restarting it again would be a second bounce for nothing.
    if matches!(client.version().await, Ok(Some(v)) if v == target_version) {
        return;
    }

    // Two failures are expected rather than exceptional, and both mean the same
    // thing: a helper older than this release answers `unknown command:
    // restart`, and a helper that would be unsafe to relaunch refuses with its
    // reason. Neither is worth a line of output, because the watcher and the
    // wait below already cover them.
    let _ = client.restart().await;
}

/// Last resort, offered only after both no-sudo paths have had their chance and
/// the helper still has not come back.
///
/// Deliberately not offered up front. `veld update` used to reach for sudo
/// first, which meant it spent a password prompt on every single update to buy a
/// determinism the socket now provides for free — and it fired before the
/// no-sudo mechanism was ever given a chance, so the prompt was almost always
/// unnecessary. A prompt that arrives after a visible failure, naming what
/// failed, is a different prompt from one that interrupts a working update.
///
/// Returns whether the helper came back healthy.
async fn offer_sudo_helper_restart(socket: &std::path::Path, target_version: &str) -> bool {
    use std::io::{BufRead, Write};

    // Free recovery FIRST, and before saying anything. `restart_privileged_helper(false)`
    // only ever runs `sudo -n`, which fails rather than asking, so it is safe on a
    // headless box and silent on an attended one — it is also what the *old* code
    // did unconditionally, and dropping it would have quietly cost every
    // NOPASSWD/cached-credential machine, and every desktop handoff that could not
    // open a terminal, a recovery they used to get for nothing. Reporting the
    // timeout before trying it would print a red failure on machines where the very
    // next line is a green success.
    if veld_core::setup::restart_privileged_helper(false).await
        && wait_for_helper_version(socket, target_version, SUDO_RESTART_TIMEOUT).await
    {
        return true;
    }

    output::print_error(
        &format!(
            "veld-helper is still not running {target_version} after {}s.",
            HELPER_RESTART_TIMEOUT.as_secs()
        ),
        false,
    );

    // Why the helper is not coming back decides whether a prompt is even honest.
    // If the newly installed binary does not run, `launchctl kill` relaunches the
    // service onto exactly that binary — which is the 2432-line crash loop, with
    // the helper then gone entirely instead of merely stale. The two no-sudo
    // paths refuse to exit onto an unrunnable binary; sudo must refuse to be
    // offered for one, or it is the hole they were closed against.
    if let Some(bad) = unrunnable_helper_binary().await {
        output::print_error(
            &format!(
                "The installed veld-helper at {} does not run, so restarting the service would \
                 leave it down rather than update it. Re-run `veld update`; if it persists, \
                 re-run `veld setup privileged`.",
                bad.display()
            ),
            false,
        );
        return false;
    }

    // A human is present only with a TTY on both ends and no explicit request to
    // run non-interactively — otherwise sudo's password prompt would hang a
    // scripted or pty-driven update. `attended()` reads VELD_NON_INTERACTIVE the
    // way the shell does: unset or empty stays interactive, any non-empty value
    // (including `0`) does not.
    if !attended() {
        println!(
            "  {}",
            output::dim(
                "Restart it with: sudo launchctl kill TERM system/dev.veld.helper \
                 (Linux: sudo systemctl restart veld-helper)"
            )
        );
        return false;
    }

    print!("  Restart the privileged helper with sudo? [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    if read == 0 {
        println!();
    }
    if !consent(if read == 0 { None } else { Some(line.as_str()) }) {
        return false;
    }

    if !veld_core::setup::restart_privileged_helper(true).await {
        return false;
    }
    wait_for_helper_version(socket, target_version, SUDO_RESTART_TIMEOUT).await
}

/// The binary the privileged helper service would be relaunched onto, if it is
/// there but does not execute.
///
/// The path comes from the **service manager**, not from `paths::lib_dir()`.
/// `lib_dir()` prefers `~/.local/lib/veld` whenever that directory merely exists,
/// while a privileged install writes `/usr/local/lib/veld` and the plist pins
/// whichever absolute path `veld setup` chose — so on a machine carrying both,
/// guessing would accuse a binary this update never touched *and*, worse, clear a
/// broken one: the sudo restart would then relaunch the root service onto exactly
/// the file the no-sudo paths had just refused, which is the crash loop this
/// guard exists to prevent.
///
/// `None` covers "it runs", "there is no such file", and "we could not tell" —
/// a probe that failed for a reason of its own, or a service manager that did not
/// answer, must not be reported to the user as a broken binary. This mirrors
/// `veld-helper`'s own `binary_executes`, and for the same reason: a stat cannot
/// tell a finished write from a paused one, and asking the kernel to exec the
/// thing can.
async fn unrunnable_helper_binary() -> Option<PathBuf> {
    let bin = veld_core::setup::privileged_helper_program().await?;
    if !bin.is_file() {
        return None;
    }
    let run = tokio::process::Command::new(&bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match tokio::time::timeout(std::time::Duration::from_secs(20), run).await {
        Ok(Ok(status)) if status.success() => None,
        // A spawn error is ENOEXEC on a half-written file — exactly the case.
        Ok(Ok(_)) | Ok(Err(_)) => Some(bin),
        // A hung probe proves nothing; do not accuse the binary.
        Err(_) => None,
    }
}

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
    use super::{
        DesktopChoice, DesktopGate, DesktopPlan, consent, desktop_gate, honour_relaunch,
        plan_desktop,
    };

    /// The whole optional-app decision, cell by cell.
    ///
    /// Every row is a user, and three of the eight cells were the bug being fixed
    /// or a bug a fix could introduce: an orchestrator-only user downloading the
    /// app on every update, an IDE user stranded on a stale app by a run that
    /// could not ask, and an app deleted from under somebody who never said no.
    #[test]
    fn the_recorded_answer_decides_the_app_half() {
        use DesktopChoice::{Unwanted, Wanted};

        // A recorded answer is obeyed whatever else is true — including on a run
        // that could have asked. Asking somebody who has already answered is the
        // nag this feature exists to avoid.
        for installed in [true, false] {
            for may_ask in [true, false] {
                assert_eq!(
                    desktop_gate(Some(Wanted), installed, may_ask),
                    DesktopGate::Proceed,
                    "wanted/{installed}/{may_ask}"
                );
                assert_eq!(
                    desktop_gate(Some(Unwanted), installed, may_ask),
                    DesktopGate::OptedOut,
                    "unwanted/{installed}/{may_ask}"
                );
            }
        }

        // Never asked, and there is somebody to ask: ask. This is where every
        // existing user lands on their first interactive `veld update` — with the
        // app installed, having never chosen it.
        assert_eq!(desktop_gate(None, true, true), DesktopGate::Ask);
        assert_eq!(desktop_gate(None, false, true), DesktopGate::Ask);

        // Never asked and nobody to ask — an agent-driven update, a CI box, the
        // app's own handoff. The app they already have is kept in step; one they
        // have never had is not downloaded.
        assert_eq!(desktop_gate(None, true, false), DesktopGate::Proceed);
        assert_eq!(desktop_gate(None, false, false), DesktopGate::SkipUnasked);
    }

    /// `plan_desktop` reads the process environment, which is global.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn plan(relaunch: bool) -> DesktopPlan {
        plan_with(true, relaunch, None)
    }

    /// `recorded` is a parameter rather than a file this reads, so nothing here
    /// depends on the maintainer's own `~/.veld/desktop.json`.
    fn plan_with(handoff: bool, relaunch: bool, recorded: Option<DesktopChoice>) -> DesktopPlan {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(plan_desktop(None, handoff, relaunch, recorded))
    }

    /// An opted-out app half must reach the app's **report**, not just be skipped.
    ///
    /// The failure this pins is the silent one: the app quits itself to hand the
    /// update over and reads `desktop-update.json` on the way back up, showing a
    /// dialog only when the report says something went wrong. So an `OptedOut`
    /// path that returned with `skipped: None` would have the app reopen on its
    /// old version having been told nothing — the same class of bug the
    /// `VELD_DESKTOP=0` test below exists for, and the reason `plan_desktop` takes
    /// the preference as an argument at all.
    #[test]
    fn an_opted_out_app_half_is_reported_to_the_app_that_quit_for_it() {
        let _guard = env_lock();
        // SAFETY: the lock above is held for the duration of this test. Cleared
        // rather than set: this asserts the *preference* path, and an ambient
        // `VELD_DESKTOP` would return above it and pass for the wrong reason.
        unsafe { std::env::remove_var("VELD_DESKTOP") };

        let opted_out = plan_with(true, true, Some(DesktopChoice::Unwanted));
        assert!(!opted_out.update, "an opted-out run must not move the app");
        assert!(
            opted_out.reopen,
            "an app that handed off and quit is owed a window back even when veld will \
             not update it",
        );

        // macOS only, for the same reason the neighbouring test scopes its
        // assertion: off macOS `plan_desktop` returns at the platform check
        // *above* the preference gate, so there is no opt-out to report.
        #[cfg(target_os = "macos")]
        assert!(
            opted_out.skipped.is_some(),
            "an opted-out app half must reach desktop-update.json, or the app reopens on \
             the old version having said nothing",
        );

        // And a terminal run owes no report — the reason was printed where the
        // user is looking. `skipped` exists only for the handoff channel.
        let from_a_terminal = plan_with(false, false, Some(DesktopChoice::Unwanted));
        assert!(!from_a_terminal.update);
        assert!(from_a_terminal.skipped.is_none());
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

#[cfg(test)]
mod handoff_tests {
    use super::{console_args, status_json};
    use veld_core::update_lock::{Origin, Phase, StaleReason, UpdateState};

    fn state() -> UpdateState {
        let t = chrono::DateTime::parse_from_rfc3339("2026-08-09T07:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        UpdateState {
            pid: 4242,
            origin: Origin::Console,
            version: Some("16.12.0".into()),
            started_at: t,
            phase: Phase::Installing,
            phase_at: t,
            tty: Some("/dev/ttys004".into()),
        }
    }

    #[test]
    fn the_window_is_never_told_to_open_a_window() {
        // The one arm that must never appear. With it, the window's `veld update`
        // opens another window, which opens another — and each one waits 20s for
        // the next to take a lock it will never get.
        let args = console_args(
            Some(4711),
            true,
            Some(std::path::Path::new("/Applications/Veld.app")),
            Some("16.12.0"),
            false,
            true,
        );
        assert!(!args.contains(&"--console".to_string()), "{args:?}");
    }

    #[test]
    fn every_flag_the_app_handed_over_reaches_the_window() {
        assert_eq!(
            console_args(
                Some(4711),
                true,
                Some(std::path::Path::new(
                    "/Applications/Veld.app/Contents/MacOS/Veld"
                )),
                Some("16.12.0"),
                true,
                true,
            ),
            vec![
                "update",
                // Without this the window asks api.github.com instead of
                // installing the release the app was actually offered.
                "--target-version",
                "16.12.0",
                // Without this the window replaces a bundle the app still holds
                // open, and install.sh's pgrep guard silently skips the app half.
                "--wait-pid",
                "4711",
                // Without this the user's app never comes back.
                "--relaunch",
                "--app-path",
                "/Applications/Veld.app/Contents/MacOS/Veld",
                "--force",
                // Without this the window the handoff opened — the surface the
                // user was sent to *watch* — is the one place `--verbose` does
                // nothing.
                "--verbose",
            ]
        );
    }

    #[test]
    fn a_terminal_run_hands_over_nothing_it_was_not_given() {
        // `veld update --console` typed by hand: no app quit for it, so no pid to
        // wait for and nothing owed a relaunch. `honour_relaunch` already refuses
        // `--relaunch` without a pid upstream of this, and passing one anyway
        // would have the window *launch* an app the user never had running.
        assert_eq!(
            console_args(None, false, None, None, false, false),
            vec!["update"]
        );
    }

    #[test]
    fn the_status_payload_keeps_the_keys_agents_are_told_to_read() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-09T07:01:40Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        // Nothing running: one key, and it is the one to branch on.
        let none = status_json(None, now);
        assert_eq!(none["in_progress"], serde_json::json!(false));
        assert_eq!(none.as_object().unwrap().len(), 1);

        let live = status_json(Some(&(state(), None)), now);
        assert_eq!(live["in_progress"], serde_json::json!(true));
        assert_eq!(live["pid"], serde_json::json!(4242));
        assert_eq!(live["origin"], serde_json::json!("console"));
        assert_eq!(live["version"], serde_json::json!("16.12.0"));
        assert_eq!(live["phase"], serde_json::json!("installing"));
        assert_eq!(live["age_seconds"], serde_json::json!(100));
        assert_eq!(live["tty"], serde_json::json!("/dev/ttys004"));
        assert_eq!(live["stale_reason"], serde_json::Value::Null);

        // A stale lock is NOT in progress, but still describes itself — a
        // consumer branching on the boolean is right, and one that wants to
        // explain the leftovers has the reason.
        let stale = status_json(Some(&(state(), Some(StaleReason::Stalled))), now);
        assert_eq!(stale["in_progress"], serde_json::json!(false));
        assert_eq!(stale["stale_reason"], serde_json::json!("stalled"));
        assert_eq!(stale["pid"], serde_json::json!(4242));
    }
}
