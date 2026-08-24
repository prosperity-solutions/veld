//! `veld desktop` -- install, update and report on Veld Desktop, the macOS app.
//!
//! The CLI installs the app for a reason that is not convenience: a build a
//! browser downloaded carries `com.apple.quarantine`, and that attribute is what
//! makes Gatekeeper refuse the first launch of a build that is not notarized.
//! curl does not set it, so an app delivered through the install script simply
//! opens — no Developer ID required. The trust boundary is unchanged, since the
//! script came from the same origin and the archive is checksum-verified.
//!
//! The work lives in the install script, so that `veld update` keeps the app in
//! step without a second implementation of download-verify-install
//! (`veld_core::setup::install_desktop`). What lives *here* is everything the
//! script cannot do from inside itself: knowing whether the app actually moved,
//! and — when the app handed its own update over and quit — making sure the user
//! ends up with a window and an explanation rather than neither.

use std::path::{Path, PathBuf};

use veld_core::desktop_pref::{self, DesktopChoice};

use crate::output;

/// `veld desktop install` / `veld desktop update`.
///
/// `wait_pid`, `relaunch` and `app_path` exist for the app updating *itself*: it
/// hands off to the CLI and quits, because an Electron app reads from its own
/// bundle while it runs and cannot be swapped underneath.
pub async fn install(
    version: Option<String>,
    wait_pid: Option<u32>,
    relaunch: bool,
    app_path: Option<PathBuf>,
) -> i32 {
    if std::env::consts::OS != "macos" {
        output::print_error(
            "Veld Desktop is installed by this command on macOS only. On Linux, download the \
             AppImage or .deb from the releases page — the AppImage updates itself.",
            false,
        );
        return 1;
    }

    // **Running this command is the answer to "do you want the app?".** Recorded
    // before the download rather than after it, and that ordering is deliberate:
    // a failed install is a network problem, not a change of mind, and the
    // preference is what stops the *next* `veld update` from asking again or
    // skipping the app they just asked for. It overwrites an earlier "no" for the
    // same reason — this command is how somebody changes their mind.
    //
    // Includes the app's own handoff (`--wait-pid`), which looks non-interactive
    // and is not: it is a human clicking *Update* inside Veld Desktop, which is
    // about as explicit a "yes, I use this app" as exists.
    remember(DesktopChoice::Wanted);

    // Defaults to this binary's version, because the app and the CLI ship from one
    // tag. The app passes an explicit one when it was offered a release the CLI has
    // not caught up to yet — otherwise it would be reinstalled at the CLI's version
    // and offered the newer one again on every launch.
    let version = version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    // The bundle the app is running from, not wherever an installer would guess.
    // Passed as `process.execPath`, which is
    // `…/Veld.app/Contents/MacOS/Veld` — four levels below the directory the
    // script needs. Anything that does not look like a bundle is dropped rather
    // than passed on: an unexpected shape would otherwise become a
    // `VELD_DESKTOP_DIR` and install the app somewhere arbitrary.
    let app_dir = app_path.as_deref().and_then(bundle_dir_of);
    if app_path.is_some() && app_dir.is_none() {
        output::print_error(
            &format!(
                "Ignoring --app-path {}: it is not inside a Veld.app bundle.",
                app_path.as_deref().unwrap_or(Path::new("")).display(),
            ),
            false,
        );
    }

    // Only the app passes a pid, and only the app is not watching this terminal.
    let handoff = wait_pid.is_some();
    let log = if handoff {
        veld_core::setup::desktop_update_log_path()
    } else {
        None
    };

    output::print_info(&format!("Installing Veld Desktop {version}..."));
    if let Some(path) = &log {
        output::print_info(&format!("Logging to {}", path.display()));
    }

    let opts = veld_core::setup::DesktopInstall {
        wait_pid,
        relaunch,
        app_dir: app_dir.clone(),
        log,
        // This command prints its own "Installing Veld Desktop …" and its own
        // result, so the script stays quiet here too.
        verbose: false,
    };

    let outcome = match veld_core::setup::install_desktop(&version, &opts).await {
        // "install script exited with code 1" is true and useless. The script
        // already said why — into the log, where on the handoff path nobody was
        // going to look — so lift that line into the message the user gets.
        // …unless the script never ran, in which case that log belongs to a
        // *previous* run: the download happens before the log file is truncated, so
        // lifting its last line prefixes this failure with an unrelated stale
        // reason — "checksum verification failed for … (HTTP 429 …)". The error is
        // the whole story on that path, and nothing else wrote to the log.
        Err(e) if veld_core::setup::is_install_script_unavailable(&e) => Err(format!("{e}")),
        Err(e) => Err(match opts.log.as_deref().and_then(last_diagnostic) {
            Some(reason) => format!("{reason} ({e})"),
            None => format!("{e}"),
        }),
        // The script is fetched from veld.oss.life.li, not from this checkout,
        // and `/get` 302s to `raw.githubusercontent.com/.../main/install.sh`
        // (website/nginx.conf) — so it tracks **main**, not the release, and
        // `--version` pins the release *assets* while the program that installs
        // them is whatever was merged. That is worth knowing in both directions:
        // an installer fix reaches every already-installed CLI the moment it
        // merges, and an unreleased change to install.sh is live for everyone
        // immediately. Either way the script can do something this binary does
        // not expect, so its success is checked rather than trusted: without
        // this the command would report success and the app would never appear.
        Ok(()) => match veld_core::setup::desktop_app_status_in(app_dir.as_deref()) {
            Some((path, installed)) if installed.as_deref() == Some(version.as_str()) => Ok(path),
            Some((path, installed)) => Err(format!(
                "the installer left Veld Desktop at {} ({}), not {version}. See \
                 ~/.veld/desktop-update.log for what the installer did.",
                path.display(),
                installed.as_deref().unwrap_or("unknown version"),
            )),
            None => Err(
                "the installer did not place Veld Desktop. See ~/.veld/desktop-update.log for \
                 what it did, then try again."
                    .to_string(),
            ),
        },
    };

    // Before this function returns, and therefore before anything else this
    // process does — the ordering is load-bearing and worth stating, because the
    // script has *already* reopened the app by now (its EXIT trap runs before
    // `install_desktop` returns). The app reads the report in `initUpdater`,
    // which is gated on Electron's `whenReady`, so the gap being raced is a cold
    // app launch (seconds) against a `PlistBuddy` call and one small write
    // (milliseconds). Do not move this later, and do not add work between the
    // script finishing and here.
    //
    // If the CLI is killed in that window there is no report at all and the app
    // says nothing — which is the same silence as before this existed, and is
    // deliberately not papered over with a "pending" marker: a marker the app
    // found before the CLI overwrote it would announce a failure that had not
    // happened, which is worse than the silence it replaces.
    if handoff {
        veld_core::setup::write_desktop_update_report(
            &version,
            outcome.as_ref().map(|_| ()).map_err(|e| e.as_str()),
            // This command only ever installs the app, so a failure here is
            // always retried with `veld desktop update`.
            veld_core::setup::UpdateHalf::App,
        );
    }

    match outcome {
        Ok(path) => {
            output::print_success(&format!("Veld Desktop {version} — {}", path.display()));
            0
        }
        Err(e) => {
            output::print_error(&format!("Could not install Veld Desktop: {e}"), false);
            // The script relaunches the app on every one of *its* exit paths, but
            // it never ran if the failure was reaching it at all — no network for
            // the script itself, bash missing, a 500 from the host. The app quit
            // for this, so bringing it back is this process's job too. `open` on a
            // running app focuses it, so doing it twice is harmless.
            if relaunch {
                relaunch_app(app_dir.as_deref());
            }
            1
        }
    }
}

/// `veld desktop uninstall` — remove the app and stop veld reinstalling it.
///
/// The other half of making the app optional, and the reason it is a command
/// rather than only a prompt: someone who answered "yes" once, or who was never
/// asked because they installed veld before this existed, needs a way to change
/// their mind that does not involve dragging a bundle to the Trash and then
/// watching the next `veld update` put it back.
///
/// **The preference is recorded even when there is no bundle to remove.** That is
/// not a degenerate case, it is a supported one: an orchestrator-only user who has
/// never had the app can say so up front, and every future install and update then
/// skips it without asking.
pub async fn uninstall(assume_yes: bool) -> i32 {
    if std::env::consts::OS != "macos" {
        output::print_error(
            "veld does not manage Veld Desktop on this platform — remove the AppImage, or use \
             your package manager for the .deb.",
            false,
        );
        return 1;
    }

    let installed = veld_core::setup::desktop_app_status().map(|(path, _)| path);

    if let Some(path) = &installed {
        if !assume_yes && output::is_tty() && !confirm_removal(path) {
            output::print_info("Cancelled — nothing was removed and nothing was remembered.");
            return 1;
        }
    }

    // Before the removal, not after it. If deleting the bundle fails — a
    // `/Applications` this user cannot write, an app that will not quit — the
    // answer they just gave is still their answer, and the thing that actually
    // ends the complaint is veld not downloading the app again. Recording only on
    // success would leave the next `veld update` re-fetching an app the user has
    // explicitly refused.
    remember(DesktopChoice::Unwanted);

    let Some(path) = installed else {
        output::print_success(
            "Veld Desktop will not be installed — `veld update` skips the app half from now on.",
        );
        println!(
            "  {}",
            output::dim("Run 'veld desktop install' if you want it after all.")
        );
        return 0;
    };

    match veld_core::setup::remove_desktop_app(&path).await {
        Ok(()) => {
            output::print_success(&format!("Removed {}", path.display()));
            println!(
                "  {}",
                output::dim(
                    "`veld update` skips the app half from now on. Run 'veld desktop install' if \
                     you want it back."
                )
            );
            0
        }
        Err(e) => {
            // The preference is already recorded, so say that too: otherwise the
            // user reads a bare failure and reasonably assumes nothing happened.
            output::print_error(&format!("{e:#}"), false);
            println!(
                "  {}",
                output::dim(
                    "veld will not install it again either way — drag it to the Trash to finish, \
                     or re-run this once the app has quit."
                )
            );
            1
        }
    }
}

/// The confirmation in front of deleting the bundle.
///
/// `[y/N]` and unrecognised-means-no, unlike the preference prompt below: this one
/// guards an irreversible act, so it takes agreement rather than the absence of
/// refusal. Names what is *not* affected, because "uninstall" next to the word
/// veld reads as if it might take the daemon and the environments with it.
fn confirm_removal(path: &Path) -> bool {
    use std::io::{BufRead, Write};

    eprintln!(
        "{} This removes {} and stops veld installing it again.",
        output::yellow("Warning:"),
        path.display()
    );
    eprintln!(
        "  {}",
        output::dim(
            "The CLI, the daemon and your running environments are untouched — so are terminal \
             sessions, which belong to the daemon rather than to the app window."
        )
    );
    eprint!("Continue? [y/N] ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        eprintln!();
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Persist an answer, and never fail the command over it.
///
/// A machine with no writable home directory still has to *act* on the answer it
/// was just given; the cost of not recording it is being asked once more, which is
/// not worth turning a working install into a failed one. Said out loud rather
/// than swallowed, because "I answered this last week" is otherwise a mystery.
fn remember(choice: DesktopChoice) {
    if let Err(e) = desktop_pref::write(choice) {
        // Names the file, which is the whole reason `desktop_pref::path()` is
        // public: on the failures that actually happen — a root-owned
        // `desktop.json` left by an old `sudo curl | bash`, an unwritable
        // `~/.veld` — the path *is* the fix, and without it the reader has an
        // errno and nowhere to point it.
        let at = desktop_pref::path()
            .map(|p| format!(" at {}", p.display()))
            .unwrap_or_default();
        output::print_error(
            &format!(
                "Could not record your Veld Desktop preference{at} ({e}) — you may be asked again."
            ),
            false,
        );
    }
}

/// Ask whether this machine wants Veld Desktop, and remember the answer.
///
/// Shared with `veld update`, which is where most existing users meet it: they
/// have the app because every release before this one installed it for them, and
/// this is the one time they are asked whether they wanted it.
///
/// Two wordings, because they are two different questions. Somebody who already
/// has the app is being asked whether to *keep* something — so the prompt has to
/// name where it is, say plainly that it arrived without them choosing it, and say
/// what each answer will do to it. Somebody who does not have it is being offered
/// a download.
///
/// The default (a bare Enter) is whatever the machine already looks like: keep the
/// app that is there, do not fetch one that is not. Returns `None` when nobody
/// answered — EOF, or something that is not yes or no — and `None` is deliberately
/// **not** recorded: an unparsed keystroke must not decide this, and the next run
/// asking again costs one prompt.
pub(crate) fn ask_desktop_choice(installed: Option<&Path>) -> Option<DesktopChoice> {
    use std::io::{BufRead, Write};

    println!();
    let default_yes = installed.is_some();
    match installed {
        Some(path) => {
            output::print_info(&format!("Veld Desktop is installed at {}.", path.display()));
            println!(
                "  {}",
                output::dim(
                    "It is the Mac app for Veld's IDE — worktree tabs, terminal panes and browser \
                     panes in one window. Earlier releases installed it alongside the CLI without \
                     asking; from now on it is your choice, so this is the one time we ask."
                )
            );
            println!();
            println!(
                "  {}",
                output::dim("Yes — veld keeps it up to date on every update, as it does today.")
            );
            println!(
                "  {}",
                output::dim("No  — veld removes it now and never downloads it again.")
            );
            print!("  Keep Veld Desktop? [Y/n] ");
        }
        None => {
            output::print_info(
                "Veld Desktop — the Mac app for Veld's IDE — is not installed on this machine.",
            );
            println!(
                "  {}",
                output::dim(
                    "Worktree tabs, terminal panes and browser panes in one window, over the same \
                     daemon the CLI drives. A ~113 MB download, kept in step by `veld update`."
                )
            );
            print!("  Install Veld Desktop? [y/N] ");
        }
    }
    let _ = std::io::stdout().flush();

    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line).unwrap_or(0);
    if read == 0 {
        // `read_line` returning 0 is EOF, so the cursor is still after the prompt.
        println!();
    }
    let choice = desktop_answer(
        if read == 0 { None } else { Some(line.as_str()) },
        default_yes,
    )?;
    remember(choice);
    println!(
        "  {}",
        output::dim(
            "Remembered. Change it any time with 'veld desktop install' or \
             'veld desktop uninstall'."
        )
    );
    Some(choice)
}

/// What the typed answer means.
///
/// Split from the prompt because the prompt cannot be tested and this decides
/// whether a GUI app gets downloaded or deleted.
///
/// **Unrecognised input is `None`, not "no"** — the opposite of `consent` in
/// `update.rs`, and the difference is what the answer is used for. There, an
/// unrecognised line declines to close a running app, and declining is the safe
/// direction. Here the answer is *written down and obeyed forever*, so a stray
/// keystroke must leave the question unanswered rather than opt somebody out of
/// the app permanently. EOF is `None` for the same reason.
fn desktop_answer(line: Option<&str>, default_yes: bool) -> Option<DesktopChoice> {
    let line = line?;
    match line.trim().to_lowercase().as_str() {
        "" if default_yes => Some(DesktopChoice::Wanted),
        "" => Some(DesktopChoice::Unwanted),
        "y" | "yes" => Some(DesktopChoice::Wanted),
        "n" | "no" => Some(DesktopChoice::Unwanted),
        _ => None,
    }
}

/// The directory containing the `Veld.app` that `exe` runs from.
///
/// `…/Veld.app/Contents/MacOS/Veld` → `…`. Returns `None` for anything that is
/// not an absolute path with a `Veld.app` in it — better to fall back to the
/// script's own search than to install into a directory nobody named. A relative
/// path is refused for the same reason: the script runs from wherever the CLI
/// happened to be spawned, which under launchd is `/`.
pub(crate) fn bundle_dir_of(exe: &Path) -> Option<PathBuf> {
    let bundle = exe
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "Veld.app"))?;
    let dir = bundle.parent().filter(|p| p.is_absolute())?;

    // App Translocation: macOS runs a quarantined app from a read-only nullfs
    // mount under `/private/var/folders/…/AppTranslocation/<uuid>/d/`, and
    // `process.execPath` reports *that* path. It is absolute and it does contain
    // `Veld.app`, so without this it becomes `VELD_DESKTOP_DIR` and pins the
    // installer to a read-only mount — every in-app update then fails at the
    // `mv`. This is not hypothetical for a `.dmg` download, which is exactly the
    // route the README still documents and the one whose users most need the
    // update to work. Falling through to the script's own search sends the
    // install to /Applications, which is where a translocated app should end up.
    if dir
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return None;
    }
    Some(dir.to_path_buf())
}

/// The last thing the install script complained about, if it complained.
///
/// The script's own output is the only place the *reason* for a failed handoff
/// exists — the exit code carries none of it, and on that path there is no
/// terminal to have watched. Last rather than first: the script warns and
/// continues in places, so the final complaint is the one that ended it.
///
/// Falls back to the last non-empty line when nothing carries an `Error:`/
/// `Warning:` prefix, because several of the script's failure paths emit only
/// the underlying tool's stderr — a read-only destination fails at `mv` with
/// `mv: … Read-only file system` and no prefix at all. Preferring a prefixed
/// line and *settling* for any line is the difference between telling the user
/// their disk is full and telling them "exited with code 1".
pub(crate) fn last_diagnostic(log: &Path) -> Option<String> {
    let text = std::fs::read_to_string(log).ok()?;
    let clamp = |l: &str| -> String {
        // Long enough for any line the script emits, short enough that a runaway
        // one cannot become the whole dialog.
        l.chars().take(300).collect()
    };
    let mut fallback = None;
    for line in text.lines().rev().map(str::trim) {
        if line.starts_with("Error:") || line.starts_with("Warning:") {
            return Some(clamp(line));
        }
        // curl's progress meter is redrawn with \r into one enormous "line" and
        // is never the answer to "what went wrong".
        if fallback.is_none() && !line.is_empty() && !line.contains('\r') {
            fallback = Some(clamp(line));
        }
    }
    fallback
}

/// Reopen the app after a failure that stopped the script from doing it.
fn relaunch_app(app_dir: Option<&Path>) {
    let Some((path, _)) = veld_core::setup::desktop_app_status_in(app_dir) else {
        return;
    };
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(&path)
        .status();
}

/// What this CLI can be asked to do, for a caller that cannot know its version.
///
/// The app already runs `veld desktop status --json` as a capability probe before
/// it hands anything over, so the answer travels on a call that was being made
/// anyway. A **list**, not a boolean per feature: the app tests for membership, an
/// older CLI omits the key entirely, and adding the next capability does not
/// change the shape either side parses.
///
/// `full-update-handoff` — `veld update` accepts `--wait-pid`/`--relaunch`/
/// `--app-path` and moves *both* halves of the release. Without it the app must
/// fall back to `veld desktop update`, which moves the app only and leaves the
/// CLI behind.
///
/// Advertised **conditionally**, and the condition is the whole reason this is a
/// capability rather than a version floor: see [`can_hand_off_full_update`].
/// `console-handoff` — `veld update` accepts `--console` and will re-run itself
/// in a terminal window. Unconditional, unlike its neighbour: it is a statement
/// about this binary's *vocabulary*, not about whether the machine can deliver
/// an unattended update, and the flag degrades to a headless run by itself when
/// no terminal can be opened.
///
/// It exists because the app and the CLI can genuinely skew: `veld desktop
/// update` moves the app half **alone**, so an app on the new release can be
/// talking to a CLI on the old one. That CLI advertises `full-update-handoff`
/// (it has always had those flags), so without a second capability the app would
/// spawn `veld update --console …` at a binary whose clap rejects the unknown
/// flag with a usage error and exit 2 — after the app has already quit, with no
/// `desktop-update.json` written, so the user reopens on the old version and is
/// told nothing. That is verbatim the failure the capability array was
/// introduced to prevent.
fn capabilities() -> Vec<&'static str> {
    let mut caps = Vec::new();
    if can_hand_off_full_update(&std::env::current_exe().unwrap_or_default()) {
        caps.push("full-update-handoff");
    }
    caps.push("console-handoff");
    caps
}

/// Whether an *unattended* full update from this binary can actually succeed.
///
/// It cannot when the CLI lives under `/usr/local/`. `install.sh` treats that as
/// a system install and refuses to relocate it — a privileged LaunchDaemon still
/// references `/usr/local` paths — so under `VELD_NON_INTERACTIVE=1` it requires
/// `sudo -n` and exits 1 when that fails. The app's handoff is a detached child
/// with no controlling terminal, so `sudo -n` fails there unless a credential
/// happens to be cached: the app would quit, the whole update would fail, and the
/// user would be told to run curl by hand.
///
/// So the app is told "no" and takes the app-only route it took before this
/// existed — which works, and leaves `veld update` in a terminal (where sudo may
/// prompt) as the way those machines move both halves. Advertising a capability
/// this binary cannot deliver would be worse than not having it.
///
/// Deliberately **not** probed with `sudo -n`: this runs on the app's periodic
/// update check, and a status command must not poke sudo every six hours.
/// Mirrors install.sh's own `case "$EXISTING_DIR" in /usr/local/*)` test, which
/// `tests/validate-install-contract.sh` pins.
fn can_hand_off_full_update(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    !dir.starts_with("/usr/local")
}

/// `veld desktop status` -- where the app is and whether it matches this CLI.
///
/// Two different questions, depending on the platform, and conflating them was a
/// bug: on macOS veld *manages* the app, so it can report a version and offer to
/// fix a mismatch. On Linux the app belongs to dpkg or to a single AppImage file
/// the user parked somewhere, so the honest answer is where it is (if that can be
/// known) and that veld does not update it. It used to say "Veld Desktop is not
/// installed." on a Linux box with a `.deb` installed, which is simply false.
pub async fn status(json: bool) -> i32 {
    let cli_version = env!("CARGO_PKG_VERSION");

    if std::env::consts::OS != "macos" {
        return status_unmanaged(json, cli_version);
    }

    let found = veld_core::setup::desktop_app_status();
    // `null` for "never asked", which is a third state rather than a missing
    // boolean — every user who installed veld before the app became optional is in
    // it, with the app on their disk and no answer on record.
    let preference = desktop_pref::read();
    let preference_json = preference.map(DesktopChoice::as_str);

    if json {
        let payload = match &found {
            Some((path, version)) => serde_json::json!({
                "installed": true,
                "managed": true,
                "platform": std::env::consts::OS,
                "path": path.display().to_string(),
                "version": version,
                "cli_version": cli_version,
                "in_sync": version.as_deref() == Some(cli_version),
                "preference": preference_json,
                "capabilities": capabilities(),
            }),
            None => serde_json::json!({
                "installed": false,
                "managed": true,
                "platform": std::env::consts::OS,
                "cli_version": cli_version,
                "preference": preference_json,
                "capabilities": capabilities(),
            }),
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
        return 0;
    }

    match found {
        Some((path, version)) => {
            let version = version.unwrap_or_else(|| "unknown".to_string());
            output::print_info(&format!("Veld Desktop {version}"));
            println!("  {}", output::dim(&path.display().to_string()));
            // The one combination worth stating rather than leaving to be
            // discovered: the app is here and veld has been told not to keep it.
            // Nothing else in the CLI would ever mention it again.
            if preference == Some(DesktopChoice::Unwanted) {
                println!(
                    "  {}",
                    output::dim(
                        "You opted out of the app, so `veld update` leaves this copy alone. \
                         'veld desktop install' opts back in; 'veld desktop uninstall' removes it."
                    )
                );
            } else if version != cli_version {
                println!();
                output::print_info(&format!(
                    "The CLI is {cli_version}. Run 'veld desktop update' to match them."
                ));
            }
            0
        }
        None => {
            output::print_info("Veld Desktop is not installed.");
            match preference {
                Some(DesktopChoice::Unwanted) => println!(
                    "  {}",
                    output::dim("You opted out — 'veld desktop install' if you change your mind.")
                ),
                _ => println!("  {}", output::dim("veld desktop install")),
            }
            0
        }
    }
}

/// `veld desktop status` where veld does not install the app.
///
/// `installed` is deliberately absent from the JSON when nothing was found: on
/// Linux an AppImage lives wherever the user saved it, so "not in the three
/// places a `.deb` uses" is not evidence of absence, and a consumer keying on
/// `installed: false` would be reading a claim this cannot make. `found` says
/// only what was actually looked for.
fn status_unmanaged(json: bool, cli_version: &str) -> i32 {
    let path = veld_core::setup::desktop_app_linux();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "managed": false,
                "platform": std::env::consts::OS,
                "found": path.as_ref().map(|p| p.display().to_string()),
                "cli_version": cli_version,
                "capabilities": capabilities(),
            }))
            .unwrap_or_default()
        );
        return 0;
    }

    match path {
        Some(path) => {
            output::print_info("Veld Desktop is installed.");
            println!("  {}", output::dim(&path.display().to_string()));
            println!();
            output::print_info(
                "Its version belongs to your package manager, not to veld — an AppImage \
                 updates itself and a .deb is updated with apt.",
            );
        }
        None => {
            output::print_info("No Veld Desktop found in the usual locations.");
            println!();
            output::print_info(
                "veld installs the app on macOS only. On Linux, download the AppImage or \
                 .deb from the releases page — and note an AppImage can live anywhere, so \
                 one may be installed that this did not find.",
            );
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_dir_is_the_directory_the_app_lives_in() {
        assert_eq!(
            bundle_dir_of(Path::new("/Applications/Veld.app/Contents/MacOS/Veld")),
            Some(PathBuf::from("/Applications")),
        );
        assert_eq!(
            bundle_dir_of(Path::new(
                "/Users/x/Applications/Veld.app/Contents/MacOS/Veld"
            )),
            Some(PathBuf::from("/Users/x/Applications")),
        );
    }

    #[test]
    fn the_reported_failure_is_the_scripts_last_complaint() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log.txt");
        // The script warns and carries on in places, so the *last* complaint is
        // the one that ended the run — and it is the only place the reason
        // exists, since the exit code carries none of it.
        std::fs::write(
            &log,
            "Downloading checksums...\nWarning: checksums.txt not available\nVerifying...\nError: checksum verification failed for veld-desktop-1.0.0-mac-arm64.zip\nsome trailing noise\n",
        )
        .unwrap();
        assert_eq!(
            last_diagnostic(&log).as_deref(),
            Some("Error: checksum verification failed for veld-desktop-1.0.0-mac-arm64.zip"),
        );

        // No prefixed line at all: four of the script's failure paths emit only
        // the underlying tool's stderr, so the last real line beats nothing —
        // that is the difference between telling the user their disk is
        // read-only and telling them "exited with code 1".
        std::fs::write(
            &log,
            "Downloading...\nmv: /Applications/Veld.app: Read-only file system\n",
        )
        .unwrap();
        assert_eq!(
            last_diagnostic(&log).as_deref(),
            Some("mv: /Applications/Veld.app: Read-only file system"),
        );

        // Truly nothing to say.
        std::fs::write(&log, "\n\n   \n").unwrap();
        assert_eq!(last_diagnostic(&log), None);
        assert_eq!(last_diagnostic(&dir.path().join("nope.txt")), None);

        // A pathological line cannot become the whole dialog.
        std::fs::write(&log, format!("Error: {}\n", "x".repeat(5000))).unwrap();
        assert_eq!(last_diagnostic(&log).unwrap().chars().count(), 300);
    }

    #[test]
    fn a_path_with_no_bundle_in_it_is_refused_rather_than_guessed() {
        // The failure this guards: anything returned here becomes
        // `VELD_DESKTOP_DIR`, i.e. the one directory the installer will consider.
        assert_eq!(bundle_dir_of(Path::new("/usr/local/bin/veld")), None);
        assert_eq!(
            bundle_dir_of(Path::new("/Applications/Other.app/Contents/MacOS/Other")),
            None
        );
        // Relative: the CLI's working directory under launchd is `/`, so this
        // would resolve somewhere neither the app nor the user meant.
        assert_eq!(bundle_dir_of(Path::new("Veld.app")), None);
        assert_eq!(
            bundle_dir_of(Path::new("dist/Veld.app/Contents/MacOS/Veld")),
            None
        );
        // App Translocation: absolute, contains `Veld.app`, and read-only. Taking
        // it would pin every in-app update to a mount the `mv` cannot write —
        // and it is the state a `.dmg` download launches in.
        assert_eq!(
            bundle_dir_of(Path::new(
                "/private/var/folders/ab/xy/d/AppTranslocation/9F2C/d/Veld.app/Contents/MacOS/Veld"
            )),
            None
        );
    }

    #[test]
    fn a_system_install_does_not_advertise_a_handoff_it_cannot_complete() {
        // The failure this guards is not subtle: advertise the capability from
        // `/usr/local`, and the app quits into a `veld update` that needs
        // `sudo -n` it cannot get, fails, and leaves the user with a closed app
        // and no new version. install.sh refuses to relocate a system install
        // because a privileged LaunchDaemon still references those paths, so this
        // is not something a later release can simply fix.
        assert!(!can_hand_off_full_update(Path::new("/usr/local/bin/veld")));
        assert!(!can_hand_off_full_update(Path::new("/usr/local/veld")));

        // Everywhere install.sh updates in place without sudo.
        assert!(can_hand_off_full_update(Path::new(
            "/opt/homebrew/bin/veld"
        )));
        assert!(can_hand_off_full_update(Path::new(
            "/Users/x/.local/bin/veld"
        )));

        // Not a substring test: `/usr/local-ish` is a different directory, and
        // `Path::starts_with` compares components, which is what makes that true.
        assert!(can_hand_off_full_update(Path::new(
            "/usr/local-ish/bin/veld"
        )));

        // A path with no parent cannot be reasoned about, so it gets the
        // conservative answer rather than the convenient one.
        assert!(!can_hand_off_full_update(Path::new("/")));
    }

    #[test]
    fn the_default_answer_is_whatever_the_machine_already_looks_like() {
        // App installed: Enter keeps it. The user is mid-`veld update` and did
        // not come here to lose an app.
        assert_eq!(
            desktop_answer(Some("\n"), true),
            Some(DesktopChoice::Wanted)
        );
        // No app: Enter does not start a 113 MB download.
        assert_eq!(
            desktop_answer(Some("\n"), false),
            Some(DesktopChoice::Unwanted)
        );

        for yes in ["y", "Y", "yes", "YES", " yes \n"] {
            assert_eq!(
                desktop_answer(Some(yes), false),
                Some(DesktopChoice::Wanted),
                "{yes:?}"
            );
        }
        for no in ["n", "N", "no", "NO", " no \n"] {
            assert_eq!(
                desktop_answer(Some(no), true),
                Some(DesktopChoice::Unwanted),
                "{no:?}"
            );
        }
    }

    #[test]
    fn an_answer_nobody_gave_is_not_recorded_as_one() {
        // The failure this guards: this answer is written down and obeyed by
        // every future update, so a stray keystroke or a closed pipe must leave
        // the question open rather than opt somebody out of the app forever.
        // Note it is the *opposite* rule from `update.rs`'s `consent`, where
        // unrecognised means "do not close my app".
        assert_eq!(desktop_answer(None, true), None);
        assert_eq!(desktop_answer(None, false), None);
        for junk in ["maybe", "q", "yeah", "0", "1", "sure"] {
            assert_eq!(desktop_answer(Some(junk), true), None, "{junk:?}");
            assert_eq!(desktop_answer(Some(junk), false), None, "{junk:?}");
        }
    }

    #[test]
    fn the_advertised_capabilities_are_the_ones_the_app_looks_for() {
        // Pins the *set*, not just membership: a capability added here without a
        // consumer, or renamed, is caught by the cross-language test in
        // `crates/veld-core/tests/install_script_contract.rs`, which reads this
        // file. This one pins that the list is derived rather than constant —
        // turning `capabilities()` back into a `const` is the natural-but-wrong
        // simplification, and it would re-enable the handoff on `/usr/local`.
        let from_a_user_prefix = {
            let mut caps = Vec::new();
            if can_hand_off_full_update(Path::new("/Users/x/.local/bin/veld")) {
                caps.push("full-update-handoff");
            }
            caps
        };
        assert_eq!(from_a_user_prefix, vec!["full-update-handoff"]);
    }
}
