// Update management for Veld Desktop.
//
// Two different mismatches reach the user from here, and they are not the same
// problem:
//
//  1. A newer *release* exists. The app checks the same GitHub releases the CLI
//     checks (`veld_core::setup::check_update`) through electron-updater's feed,
//     and either installs it or points at the download — see `updateMode` in
//     `updatePolicy.js` for why that differs per platform.
//  2. The app and the daemon are on different versions. They ship from one tag,
//     so this means one half updated and the other did not, and the fix depends
//     on which half: the app updates itself, the daemon updates via
//     `veld update`.
//
// Noise budget: an automatic check is silent unless it finds something, never
// prompts twice for the same version, and never reports its own failures (a
// laptop is offline more often than a release is broken). A check the user asked
// for reports every outcome, including "you're up to date".

const { execFile, spawn } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const { promisify } = require("node:util");
const path = require("node:path");

const execFileAsync = promisify(execFile);

const { Notification, app, dialog, shell } = require("electron");
const { autoUpdater } = require("electron-updater");

const {
  DEFAULT_UPDATE_FREQUENCY,
  FULL_UPDATE_HANDOFF,
  capabilitiesFrom,
  cliCandidatePaths,
  declineHolds,
  downloadOnlyReason,
  handoffCommand,
  looksLikeVeldCli,
  primaryAction,
  pruneUpdateState,
  releaseAgeMs,
  releasePageUrl,
  reportIsFresh,
  shouldOfferUpdate,
  updateFrequencyFrom,
  updateInProgress,
  updateMode,
  updatePhaseLabel,
  updateTier,
  versionSkew,
  versionsAhead,
} = require("./updatePolicy");

/** Let the window come up first — an update prompt is never the reason someone opened the app. */
const FIRST_CHECK_DELAY_MS = 15_000;

/**
 * The PATH the CLI is handed, rather than the one this process happens to have.
 *
 * A launchd-started app's PATH is whatever launchd felt like; a Finder-started
 * one inherits from `loginwindow`. `install.sh` reads the environment it is given
 * — it is where `curl`, `ditto`, `shasum` and `PlistBuddy` come from — so leaving
 * that to chance is leaving the install to chance. Everything the desktop path of
 * the script uses lives in these four directories.
 */
const SAFE_PATH = "/usr/bin:/bin:/usr/sbin:/sbin";

/** Where `veld desktop update` leaves word about how it went. */
const updateReportPath = () =>
  path.join(os.homedir(), ".veld", "desktop-update.json");

/**
 * Where the handed-off update writes what it did.
 *
 * The same path `veld_core::setup::desktop_update_log_path` returns, because the
 * failure report the app shows offers to reveal it and the CLI names it in its
 * own errors. Two constants for one file, in two languages — the pairing is
 * pinned by `crates/veld-core/tests/install_script_contract.rs`.
 */
const updateLogPath = () =>
  path.join(os.homedir(), ".veld", "desktop-update.log");

/**
 * stdout/stderr handles for the handed-off CLI, as a `[stdout, stderr]` pair.
 *
 * Truncating (`"w"`) rather than appending: this answers "what happened the last
 * time", which is exactly what the app reads back on relaunch, and an appending
 * log would have the *previous* failure sitting above the current success.
 *
 * Falls back to discarding the output if the file cannot be opened — a log that
 * will not open is not a reason to refuse an update, and everything downstream
 * already treats a missing log as "no diagnostic available".
 *
 * @returns {[number | "ignore", number | "ignore"]}
 */
function handoffLogHandles() {
  try {
    const file = updateLogPath();
    fs.mkdirSync(path.dirname(file), { recursive: true });
    const fd = fs.openSync(file, "w");
    return [fd, fd];
  } catch {
    return ["ignore", "ignore"];
  }
}

/**
 * `~/.veld/update.lock/state.json` — what the running `veld update` is doing.
 *
 * Written by `veld_core::update_lock`. A directory rather than a plain file
 * because `mkdir` is the create-or-fail primitive the lock is built on; only the
 * state inside it is read here.
 */
const updateLockDir = () => path.join(os.homedir(), ".veld", "update.lock");
const updateLockStatePath = () => path.join(updateLockDir(), "state.json");

/**
 * How long the "Veld is updating" dialog may hold the app open.
 *
 * Bounded because the dialog is shown *before* any window exists, so it can sit
 * unfocused behind other apps indefinitely — while this process keeps its bundle
 * open and `install.sh`'s `pgrep` guard skips the app half of the update.
 * Comfortably long enough to read three lines, far shorter than the ~70 s the
 * installer spends downloading before it checks.
 */
const QUIT_DIALOG_GRACE_MS = 10_000;

/**
 * The update that is running right now, if one is.
 *
 * Synchronous on purpose: the one caller runs before any window exists, and the
 * decision it feeds is whether to build a window at all.
 *
 * @returns {{pid: number, phase: string, version: string | null, origin: string} | null}
 */
function runningUpdate() {
  let state;
  try {
    state = JSON.parse(fs.readFileSync(updateLockStatePath(), "utf8"));
  } catch {
    // No lock, unreadable lock, or half-written JSON — all "no update". The
    // consequence of this answer is that the app opens, which is the safe
    // direction: the failure it guards against costs a confusing session, while
    // being wrong the other way makes the app unopenable.
    return null;
  }
  return updateInProgress({
    state,
    // `kill(pid, 0)` throws ESRCH for a pid that is gone and EPERM for one this
    // user may not signal. EPERM means it exists, which is the question.
    pidAlive: (pid) => {
      try {
        process.kill(pid, 0);
        return true;
      } catch (err) {
        return err?.code === "EPERM";
      }
    },
  });
}

/**
 * Refuse to open while an update is replacing this app's own bundle.
 *
 * The app is *supposed* to be closed for that window — `veld update` quits it,
 * swaps `/Applications/Veld.app`, and reopens it at the end. A copy launched from
 * the Dock in between attaches to a daemon that is mid-restart, holds open a
 * bundle the installer is about to replace, and (because `install.sh` refuses to
 * swap a running app) can silently reduce a full update to a CLI-only one. The
 * user's mental model is already right — they are waiting for an update — so the
 * honest response is to say so and quit rather than to open a window that will
 * misbehave.
 *
 * Deliberately **only at startup**. An update started from a terminal asks the
 * user's permission before closing this app, and auto-closing a running window
 * mid-session would overrule an answer they were explicitly asked for.
 *
 * @returns {Promise<boolean>} whether the app should stop launching
 */
async function quitIfUpdating() {
  const update = runningUpdate();
  if (!update) return false;
  const version = update.version ? ` to ${update.version}` : "";
  const shown = dialog.showMessageBox({
    type: "info",
    message: `Veld is updating${version}.`,
    detail:
      `The update is ${updatePhaseLabel(update.phase)} and will reopen Veld Desktop when it ` +
      "finishes.\n\nOpening the app now would hold on to the bundle the update is replacing." +
      // The escape hatch, named here because this is the only place the user
      // sees when it goes wrong. A lock is written off once its holder is gone
      // or after 30 minutes without progress, so the stuck case is bounded
      // rather than permanent — but a pid that got recycled inside that window
      // would otherwise leave someone with an app that will not open and
      // nothing to read about why.
      `\n\nIf no update is really running, delete ${updateLockDir()} — or run ` +
      "`veld update --status` to see what veld thinks is happening. A stalled " +
      "lock clears itself after 30 minutes.",
    buttons: ["OK"],
  });
  // **Raced, not awaited.** The bundle stays open for as long as this dialog
  // does, and `install.sh` re-checks `pgrep` immediately before the swap — so a
  // dialog nobody clicks (it appears before any window exists, so it can sit
  // unfocused behind other apps) would make the installer skip the app half and
  // quietly demote a full update to a CLI-only one. That is the exact failure
  // this function exists to prevent. The user gets long enough to read it; the
  // update gets its bundle back regardless.
  await Promise.race([shown, new Promise((r) => setTimeout(r, QUIT_DIALOG_GRACE_MS))]);
  app.quit();
  return true;
}

/**
 * Where the veld CLI is, if this machine has one.
 *
 * A GUI app does not inherit the shell's PATH — a launchd-started app gets a bare
 * one — so `which veld` is not a question that can be asked here. These are the
 * directories `install.sh` writes to, which is the same reasoning the daemon's
 * own PATH handling uses: look where the installer puts things, not where a login
 * shell would have found them.
 *
 * Resolved lazily and re-resolved per check, never at startup: the probes below
 * spawn processes, and a session outlives a `veld update` that moves the CLI.
 *
 * Two claims worth reading precisely, both of which live in `updatePolicy.js`
 * with tests: the candidate order matches `install.sh`'s own preference and is
 * **not** a trust boundary (anything able to write to any of these can replace
 * the real veld), and the `--version` check cannot stop a wrong binary from
 * running — it is performed by running it — but it does stop one from being
 * re-spawned detached with the app quitting behind it.
 *
 * Carries the CLI's advertised capabilities alongside its path, because the two
 * are learned from the same probe and must not drift apart: deciding *which*
 * command to spawn from a capability list resolved at some earlier moment, for a
 * binary that has since been replaced, is how the app ends up spawning a flag the
 * CLI on disk does not accept.
 *
 * @type {{path: string, capabilities: string[]} | null}
 */
let cli = null;

/**
 * Whether `candidate` is really the veld CLI.
 *
 * Asynchronous, like every probe here: these run on the main thread, and a
 * candidate that hangs would freeze the window rather than just the check.
 *
 * @param {string} candidate
 * @returns {Promise<boolean>}
 */
async function isVeldCli(candidate) {
  try {
    fs.accessSync(candidate, fs.constants.X_OK);
  } catch {
    return false;
  }
  try {
    const { stdout } = await execFileAsync(candidate, ["--version"], {
      encoding: "utf8",
      timeout: 2000,
      // A bare launchd PATH is enough for `--version`, and an inherited one
      // would let the environment decide what a subprocess of this resolves to.
      env: { PATH: SAFE_PATH },
    });
    return looksLikeVeldCli(stdout);
  } catch {
    return false;
  }
}

/**
 * Whether this veld can actually carry out the handoff.
 *
 * Being veld is not enough: `veld desktop` is newer than the app, and the two
 * halves do come apart — a `.dmg` download, or a `veld update` that failed after
 * the app half. Such a CLI exits 2 on an unknown subcommand, and by then the app
 * has already quit for it, so the user is left with no window, no relaunch and
 * no report. That is the precise failure this file exists to eliminate, and it
 * was reachable on the one path where the app is already gone.
 *
 * A capability probe rather than a version floor, deliberately: the release that
 * introduces `veld desktop` is not knowable while writing this, and a hardcoded
 * number would be a second thing to keep in step. `desktop status` reads a plist
 * and nothing else — no daemon, no database, no network — so asking is free.
 *
 * @param {string} candidate
 * @returns {Promise<boolean>}
 */
async function cliHandlesDesktop(candidate) {
  try {
    const { stdout } = await execFileAsync(
      candidate,
      ["desktop", "status", "--json"],
      {
        encoding: "utf8",
        // Longer than the `--version` probe: this one shells out to PlistBuddy.
        timeout: 5000,
        env: { PATH: SAFE_PATH },
      },
    );
    // The same call answers both questions — "does this CLI know `desktop` at
    // all" and "what else can it be asked to do" — so learning the second costs
    // nothing. An older CLI omits `capabilities` and comes back as `[]`, which is
    // the honest answer for it.
    return capabilitiesFrom(stdout);
  } catch {
    // Non-zero exit (clap's 2 for an unknown subcommand), or a timeout.
    return null;
  }
}

/**
 * The CLI to hand an update to, and what it is able to do with it.
 *
 * @returns {Promise<{path: string, capabilities: string[]} | null>}
 */
async function findCli() {
  for (const candidate of cliCandidatePaths({ home: app.getPath("home") })) {
    if (!(await isVeldCli(candidate))) continue;
    const capabilities = await cliHandlesDesktop(candidate);
    if (capabilities) return { path: candidate, capabilities };
  }
  return null;
}

/** @type {"off" | "install" | "download" | "cli"} */
let mode = "off";
/** Versions already offered this session, so a periodic check can't re-ask. */
const offered = new Set();
/** Daemon versions already reported as skewed, same reason. */
const skewReported = new Set();
/** `desktop.updateFrequency`, last read from the daemon. */
let frequency = DEFAULT_UPDATE_FREQUENCY;
/** Where `GET /api/settings` lives, from `initUpdater`. */
let settingsUrl = null;
/** Where the nudge state is kept, from `initUpdater`. */
let nudgeStateFile = null;
/** @type {{lastPromptedAt: number | null, seen: Record<string, number>, declined: Record<string, number>} | null} */
let nudges = null;
/** @type {NodeJS.Timeout | null} */
let checkTimer = null;
/** When {@link checkTimer} is due to fire, so a re-arm can never push it later. */
let checkDueAt = null;
let checking = false;
/** Set across `quitAndInstall` — see the error listener in `initUpdater`. */
let installing = false;
/** Version being installed, for the failure dialog's download link. */
let installingVersion = null;
/** @type {{behind: "daemon" | "app", appVersion: string, daemonVersion: string} | null} */
let currentSkew = null;
/** @type {(() => void) | null} */
let onSkewChange = null;
/** @type {(() => void) | null} */
let onQuitCancelled = null;

/**
 * What this install has already been told about, across restarts.
 *
 * Session memory was not enough once the tiers existed, and the gap is the whole
 * complaint: `offered` is a `Set` that dies with the process, so a machine that
 * reopens Veld Desktop twice a day was asked about the same declined release
 * twice a day, and a "at most one prompt per day" promise made against
 * in-memory state would be a promise about uptime rather than about days.
 *
 * Read lazily and pruned to the running version on that first read — once per
 * process, which is every time it can matter, since a running app's version
 * cannot change under it. That is what stops the file accumulating and stops a
 * downgrade resurrecting a count. A missing, unreadable or
 * malformed file is "nothing known" — the same direction everything else in this
 * file takes with `~/.veld` state, because the cost of forgetting is one extra
 * prompt and the cost of throwing is an updater that never runs again.
 */
function readNudges() {
  if (nudges) return nudges;
  let parsed = null;
  try {
    if (nudgeStateFile) parsed = JSON.parse(fs.readFileSync(nudgeStateFile, "utf8"));
  } catch {
    parsed = null;
  }
  nudges = pruneUpdateState(parsed, app.getVersion());
  return nudges;
}

/**
 * Persist the nudge state, best-effort.
 *
 * A write that fails costs the user a repeated prompt after a restart, which is
 * exactly the thing this file is here to reduce — but it is not a reason to
 * refuse an update, and `userData` being unwritable is a problem the update
 * dialog cannot fix. The in-memory copy stays authoritative for the session
 * either way.
 */
function saveNudges() {
  if (!nudgeStateFile || !nudges) return;
  try {
    fs.mkdirSync(path.dirname(nudgeStateFile), { recursive: true });
    // Write-then-rename, like the two `userData` files this sits beside
    // (`windows.js`'s `windows.json`, `browserViews.js`'s `permissions.json`) and
    // for the same reason they give: a torn file parses as nothing, which here
    // means every decline and the one-prompt-a-day floor are gone — the app
    // starts asking again, which is exactly the behaviour this file prevents.
    const tmp = `${nudgeStateFile}.tmp`;
    fs.writeFileSync(tmp, `${JSON.stringify(nudges, null, 2)}\n`);
    fs.renameSync(tmp, nudgeStateFile);
  } catch (err) {
    console.error("[veld] could not save update nudge state", err);
  }
}

/**
 * Re-read `desktop.updateFrequency` from the daemon.
 *
 * Its own request rather than a value threaded through `main.js`, because the
 * updater is the only thing that reads this key and the schedule has to survive
 * a daemon that was not up when the app started. The same 2 s budget the tray's
 * settings read uses; a failure leaves the last known tier in place rather than
 * snapping back to the default, so a daemon restart does not silently re-tier a
 * machine that had chosen `relaxed`.
 */
async function readFrequency() {
  if (!settingsUrl) return frequency;
  try {
    const res = await fetch(settingsUrl, { signal: AbortSignal.timeout(2000) });
    if (!res.ok) return frequency;
    frequency = updateFrequencyFrom(await res.json(), frequency);
  } catch {
    // Offline, or no daemon yet. Keep what we had.
  }
  return frequency;
}

/**
 * Arm the next automatic check.
 *
 * A self-rescheduling timeout rather than `setInterval`, because the period is
 * now a setting: an interval fixed at startup would keep a machine on the tier
 * it had when the app launched, and "I changed it and nothing happened" is the
 * failure a settings screen must not have. Each check re-reads the tier and
 * re-arms at its interval, and {@link settingsChanged} re-arms immediately when
 * the user actually moves the control.
 *
 * @param {number} delayMs
 */
function armCheck(delayMs) {
  if (checkTimer) clearTimeout(checkTimer);
  checkDueAt = Date.now() + delayMs;
  checkTimer = setTimeout(() => {
    checkTimer = null;
    checkDueAt = null;
    void checkForUpdates({ manual: false }).catch((err) => {
      // A `setInterval` could not stop running; a chain of timeouts can. Without
      // this, one unexpected rejection — a dialog that fails to open, anything
      // that throws before the re-arm inside the check — ends automatic updates
      // for the life of the process, silently. Re-arm only if nothing else has.
      console.error("[veld] update check failed", err);
      if (!checkTimer) armCheck(updateTier(frequency).checkIntervalMs);
    });
  }, delayMs);
  // Nothing here should hold the process alive on its own.
  checkTimer.unref?.();
}

/**
 * Re-read the tier and re-arm, after the renderer says a setting changed.
 *
 * Rides the nudge `main.js` already forwards for `desktop.menuBarIcon`, so
 * moving the control takes effect at once instead of at the end of an interval
 * that may be twelve hours long. It deliberately does *not* run a check: the
 * user changed a preference, they did not ask to be told about a release.
 */
async function settingsChanged() {
  if (mode === "off") return;
  const before = frequency;
  await readFrequency();
  if (frequency === before) return;
  // **Never later than what is already armed.** The renderer sends this nudge as
  // soon as the settings document loads, which on a normal launch is inside the
  // fifteen seconds `initUpdater` armed the first check for — so a plain re-arm
  // would cancel that first check and replace it with the tier's full interval:
  // an hour on `eager`, twelve on `relaxed`. Only `balanced` escaped, because it
  // equals the in-memory default and took the early return above. Shortening is
  // still allowed, which is what a move to a faster tier should do.
  const remainingMs = checkDueAt === null ? Number.POSITIVE_INFINITY : checkDueAt - Date.now();
  armCheck(Math.max(0, Math.min(remainingMs, updateTier(frequency).checkIntervalMs)));
}

/**
 * Wire up the updater and start the background schedule.
 *
 * @param {{onSkewChange?: () => void, onQuitCancelled?: () => void, settingsUrl?: string, stateFile?: string}} [opts]
 *   `onSkewChange` fires when the daemon-skew notice appears or clears, so the
 *   tray menu can re-render without polling for it. `onQuitCancelled` fires
 *   when an install failed *after* `quitAndInstall` was called — the app asked
 *   to quit, `before-quit` ran, and then it kept running, which is a state the
 *   rest of the shell has to be told about rather than infer. `settingsUrl` is
 *   `GET /api/settings`, where `desktop.updateFrequency` lives; `stateFile` is
 *   where the cross-restart nudge state is kept. Both are passed in rather than
 *   derived here for the same reason `windows.js` takes its own state file:
 *   `main.js` owns what the app's paths are.
 */
function initUpdater(opts = {}) {
  onSkewChange = opts.onSkewChange ?? null;
  onQuitCancelled = opts.onQuitCancelled ?? null;
  settingsUrl = opts.settingsUrl ?? null;
  nudgeStateFile = opts.stateFile ?? null;
  // Deliberately does NOT resolve the CLI here. `findCli` spawns up to three
  // candidates × two probes, and this runs inside `whenReady`, before the first
  // window exists — to answer a question nothing needs for another fifteen
  // seconds. It used to do it synchronously, which on the pathological path put
  // twenty seconds of `execFileSync` in front of the user's window; the probes
  // are async now, but the right fix is still not to ask yet.
  //
  // `updateMode` returns `"off"` exactly when the build is unpackaged, which
  // depends on `isPackaged` alone — so a provisional call with `cli: null` gets
  // the one answer this function acts on right, and `checkForUpdates` resolves
  // properly before it needs the rest.
  cli = null;
  mode = updateMode({
    platform: process.platform,
    isPackaged: app.isPackaged,
    env: process.env,
    cli: null,
  });

  // Every download in this file is one the user agreed to in a dialog first —
  // including on the platforms that *can* self-install, because applying an
  // update means restarting the app, and a terminal pane full of work is not
  // something to interrupt unannounced.
  autoUpdater.autoDownload = false;
  autoUpdater.autoInstallOnAppQuit = false;

  // Not optional, and not only for reporting: electron-updater routes every
  // failure through `dispatchError` (`AppUpdater.js` → `emit("error")`), and an
  // EventEmitter with no `error` listener *throws* the error instead of
  // delivering it. `checkForUpdates()` and `downloadUpdate()` also reject, so
  // their callers below report those in context — but `quitAndInstall()` is
  // synchronous and returns void, so on that path the throw escapes into a
  // floating promise and the user is told nothing at all. That is the worst
  // possible place for it: `AppImageUpdater.doInstall` unlinks the running
  // AppImage *before* the move that can fail, so a silent failure there is an
  // app that deleted itself and then said nothing.
  autoUpdater.on("error", (err) => {
    if (!installing) return; // the awaiting caller reports it with context
    installing = false;
    // The lock was handed to a successor that never arrived: `install()` can
    // return false without quitting, and this process then keeps running with
    // no single-instance lock — so a second launch would open a second window
    // against the same daemon instead of focusing this one. Re-take it; the
    // successor cannot be holding it, or we would not be here.
    app.requestSingleInstanceLock();
    // Same shape of recovery as the lock above: `quitAndInstall` already fired
    // `before-quit`, and the app is now staying up. Anything that latched on
    // that signal — window persistence, and the hand-back of a detached
    // window's tabs — has to be un-latched, or it stays frozen for the rest of
    // the session.
    onQuitCancelled?.();
    void reportInstallFailure(err);
  });

  autoUpdater.on("update-downloaded", (info) => {
    void promptRestart(info?.version);
  });

  // Before the mode gate: an unpackaged dev run is `"off"`, but a report can
  // only have been written by a packaged one, and a failure the user never hears
  // about is the failure mode this whole file is about.
  void reportPreviousCliUpdate();

  if (mode === "off") return;

  // The first check keeps its fixed fifteen seconds on every tier. The tier
  // governs how often a *release* is put in front of somebody, not how long the
  // app waits before knowing one exists — and knowing early is what lets the
  // `balanced` tier start a release's ripening clock on the day it lands rather
  // than on the day it is finally offered.
  armCheck(FIRST_CHECK_DELAY_MS);
}

/**
 * @param {{manual: boolean}} opts `manual: true` is a user-initiated check, which
 *   reports outcomes an automatic one swallows.
 */
async function checkForUpdates({ manual }) {
  // Re-resolve per check rather than once per session. A session outlives a
  // `veld update` that moves the CLI, and a first launch before the CLI exists
  // (the app installed first, from a .dmg) would otherwise stay in `"download"`
  // mode for as long as the app is open — telling the user, wrongly, that it
  // cannot update itself.
  if (mode !== "off") {
    cli = await findCli();
    mode = updateMode({
      platform: process.platform,
      isPackaged: app.isPackaged,
      env: process.env,
      cli: cli?.path ?? null,
    });
  }

  if (mode === "off") {
    if (manual) {
      await dialog.showMessageBox({
        type: "info",
        message: "This is an unpackaged build.",
        detail:
          "Updates apply to installed copies of Veld Desktop. Run `git pull` instead.",
        buttons: ["OK"],
      });
    }
    return;
  }

  // Re-read the tier per check, for the same reason the mode is re-resolved
  // above: a setting the user changed in a browser tab has to reach the app, and
  // the renderer's nudge only arrives when a Veld window is open. Re-arming here
  // rather than in `initUpdater` is what makes the interval a setting at all.
  await readFrequency();
  const tier = updateTier(frequency);
  // Automatic checks only. A manual *Check for Updates…* answers a question the
  // user asked; it is not the background channel and must not reset its clock,
  // or someone who checks by hand on `relaxed` would never get an automatic
  // check at all. Same reasoning as not spending the prompt budget below.
  if (!manual) armCheck(tier.checkIntervalMs);

  // Guards the network check only, never the dialogs that follow it — and
  // cleared before any of them opens, including the failure one. A `finally`
  // would not do: it runs after the `catch` block, which awaits its dialog, so
  // a second *Check for Updates…* while "Could not check for updates." is on
  // screen would return in silence — from the one path that promises to report
  // every outcome.
  if (checking) return;
  checking = true;
  /** @type {import("electron-updater").UpdateCheckResult | null} */
  let result = null;
  try {
    result = await autoUpdater.checkForUpdates();
    checking = false;
  } catch (err) {
    checking = false;
    if (manual) {
      await dialog.showMessageBox({
        type: "warning",
        message: "Could not check for updates.",
        detail: String(err?.message ?? err),
        buttons: ["OK"],
      });
    }
    return;
  }

  const version = result?.updateInfo?.version;
  if (!result?.isUpdateAvailable || !version) {
    if (manual) {
      await dialog.showMessageBox({
        type: "info",
        message: `Veld Desktop ${app.getVersion()} is up to date.`,
        buttons: ["OK"],
      });
    }
    return;
  }
  // A manual check re-offers a version already declined — asking is the whole
  // point of clicking the item — while the periodic one stays quiet.
  if (!manual && offered.has(version)) return;

  // Record the sighting before deciding anything with it. This is what starts a
  // release's ripening clock and what `versionsAhead` counts, so it has to
  // happen on the check that *declines* to prompt — that is the whole case it
  // exists for.
  const state = readNudges();
  const now = Date.now();
  if (!state.seen[version]) {
    state.seen[version] = now;
    saveNudges();
  }

  const { offer } = shouldOfferUpdate({
    tier,
    ageMs: releaseAgeMs({
      releaseDate: result?.updateInfo?.releaseDate,
      firstSeenAt: state.seen[version],
      now,
    }),
    ahead: versionsAhead({ seen: state.seen, currentVersion: app.getVersion() }),
    lastPromptedAt: state.lastPromptedAt,
    declinedAt: state.declined[version] ?? null,
    manual,
    now,
  });
  if (!offer) return;

  offered.add(version);
  // The tier's prompt gap is about the *automatic* channel, which is what the
  // setting's help text promises. A manual check is the user asking, so it does
  // not spend that budget — and it cannot cause a repeat either, because the
  // dialog it opens records a decline of its own if they say Later.
  if (!manual) {
    state.lastPromptedAt = now;
    saveNudges();
  }
  await offerUpdate(version);
}

async function offerUpdate(version) {
  const canInstall = mode === "install";
  const viaCli = mode === "cli";
  // Whether the one button on offer moves the whole release or only this app.
  // It decides the wording, so it is resolved before the dialog rather than
  // inside the handler: a prompt that says "veld" and then updates the app alone
  // is worse than one that never claimed to.
  const full =
    viaCli && (cli?.capabilities ?? []).includes(FULL_UPDATE_HANDOFF);

  let detail;
  if (full) {
    // Says what actually happens, because it is unusual: the app closes *before*
    // the download, and something outside it does the work.
    detail = `You're on ${app.getVersion()}. Veld quits, the veld CLI installs the release — CLI, daemon and app together — and the app reopens.\n\nNothing in flight is lost: terminal sessions belong to the daemon and reattach with their scrollback.`;
  } else if (viaCli) {
    detail = `You're on ${app.getVersion()}. Veld quits, the veld CLI installs the app, and it reopens.\n\nThis veld CLI updates the app only — run \`veld update\` afterwards to move the rest of the release.`;
  } else if (canInstall) {
    detail = `You're on ${app.getVersion()}. Downloading takes a moment; the app restarts to apply it.`;
  } else {
    detail = `You're on ${app.getVersion()}. ${downloadOnlyReason({ platform: process.platform })} The release page has the download.\n\nUpdate the veld CLI separately with \`veld update\`.`;
  }

  // The knob, named at the moment somebody might want it — which is while an
  // update dialog is on screen, not while reading a settings list. Withheld on
  // the quietest tier: offering to make it quieter is noise to the one person
  // who has already answered the question.
  if (frequency !== "relaxed") {
    detail += "\n\nAsked too often? Settings → General → “How eagerly to offer updates”.";
  }

  const { response } = await dialog.showMessageBox({
    type: "info",
    // Named for what is actually being offered. The feed's version is the
    // *release*, and on the path that installs the whole thing, calling it a
    // Veld Desktop update undersells it and leaves the user believing the CLI
    // still needs a separate trip to a terminal.
    message: full
      ? `veld ${version} is available.`
      : `Veld Desktop ${version} is available.`,
    detail,
    buttons: [primaryAction({ viaCli, canInstall, full }), "Later"],
    defaultId: 0,
    cancelId: 1,
  });
  if (response !== 0) {
    // "Later" now means later, not "until the next launch". Declining used to
    // live in a `Set` that died with the process, so quitting and reopening —
    // which is most of what people do with a desktop app — asked again about the
    // release they had just turned down. A *manual* check still re-offers it,
    // and so does the next release, because `pruneUpdateState` drops every
    // version the installed one has caught up with.
    const state = readNudges();
    state.declined[version] = Date.now();
    saveNudges();
    return;
  }

  if (viaCli) {
    await updateViaCli(version);
    return;
  }

  if (!canInstall) {
    await shell.openExternal(releasePageUrl(version));
    return;
  }

  notify("Downloading update", `Veld Desktop ${version}`);
  try {
    await autoUpdater.downloadUpdate();
  } catch (err) {
    await dialog.showMessageBox({
      type: "warning",
      message: "The update could not be downloaded.",
      detail: `${String(err?.message ?? err)}\n\nThe release page has the download.`,
      buttons: ["OK"],
    });
  }
}

/**
 * Hand the update to the veld CLI and get out of its way.
 *
 * The order is the whole design. An Electron app reads from its own bundle while
 * it runs — the asar, the framework dylibs — so the bundle cannot be replaced
 * underneath it. So this spawns the CLI *detached*, tells it which pid to wait
 * for, and quits: the CLI outlives the app, waits for the process to actually be
 * gone, swaps the bundle and reopens it.
 *
 * Detached and with its streams let go, or the child would die with its parent
 * and take the update with it — the app is the parent, and the app is about to
 * quit on purpose.
 */
async function updateViaCli(version) {
  // Re-resolved, not the value from `initUpdater`: a session lasts days, and
  // `veld update` moving the CLI from `~/.local/bin` to `/usr/local/bin` (or
  // an uninstall) between then and now would have this spawn a path that is no
  // longer there — or, worse, one something else has since put back.
  cli = await findCli();
  if (!cli) {
    mode = updateMode({
      platform: process.platform,
      isPackaged: app.isPackaged,
      env: process.env,
      cli: null,
    });
    await dialog.showMessageBox({
      type: "warning",
      message: "The veld CLI is no longer installed.",
      detail:
        "Veld Desktop updates through the CLI. Reinstall it with\n\ncurl -fsSL https://veld.oss.life.li/get | bash",
      buttons: ["OK"],
    });
    return;
  }

  // Nothing has been consumed yet, but a report left by a *previous* update
  // would be read as this one's the moment the app comes back. Clear it before
  // handing over, so an absent report means "the CLI never got that far".
  try {
    fs.rmSync(updateReportPath(), { force: true });
  } catch {
    // A report we cannot clear is not a reason to refuse the update.
  }

  // Chosen from the capabilities of the binary just resolved, not from the ones
  // resolved when the dialog opened — `veld update` may have moved the CLI in
  // between. See `handoffCommand` for what each shape does and why the older one
  // needs `--version` while the newer one must not have it.
  const { args, full } = handoffCommand({
    capabilities: cli.capabilities,
    version,
    pid: process.pid,
    execPath: process.execPath,
  });

  /** @type {import("node:child_process").ChildProcess} */
  let child;
  try {
    child = spawn(
      cli.path,
      args,
      {
        detached: true,
        // Everything the CLI says goes to one file, in order — its own progress
        // lines and the install script's output alike. On this path there is no
        // terminal by construction, and `stdio: "ignore"` used to mean the only
        // record of a full-release update was whatever the script itself chose to
        // log.
        //
        // **Only on the full route.** `veld desktop update` opens the same file
        // itself (`desktop.rs` → `desktop_update_log_path`, then `File::create`
        // in `run_install_script`), so handing it these descriptors puts two
        // independent open descriptions with independent offsets on one path: the
        // child's truncate discards what the parent already wrote and the
        // parent's later writes land past the child's, through output that
        // `last_diagnostic` then reads to build the user-visible failure reason.
        // The old route logs itself; leave it to it.
        stdio: ["ignore", ...(full ? handoffLogHandles() : ["ignore", "ignore"])],
        // A deliberate PATH (see `SAFE_PATH`) *over* the inherited environment,
        // not instead of it. Replacing the whole environment looked safer and
        // was worse: it dropped `TMPDIR`, `HTTPS_PROXY`/`NO_PROXY` and the
        // locale, so the installer would fail to download behind a corporate
        // proxy on the one path where nobody is watching a terminal — while the
        // same `veld desktop update` typed into a shell worked. Nothing here
        // needs to filter `VELD_*` either: `run_install_script` clears the
        // handoff variables itself and sets the rest explicitly, which is where
        // that belongs, and the CLI is the same binary the user runs by hand.
        env: { ...process.env, PATH: SAFE_PATH },
      },
    );
  } catch (err) {
    await dialog.showMessageBox({
      type: "warning",
      message: "The update could not be started.",
      detail: `${String(err?.message ?? err)}\n\nRun \`veld update\` in a terminal instead.`,
      buttons: ["OK"],
    });
    return;
  }

  // Quit only once the child is actually running. `spawn` reports an
  // asynchronous failure (ENOENT between the check above and the exec) through
  // `error`, and the old code called `app.quit()` immediately — so the app was
  // already tearing down before the event could arrive, and a failed handoff
  // took the window with it and said nothing.
  await new Promise((resolve) => {
    let settled = false;
    const done = (fn) => {
      if (settled) return;
      settled = true;
      fn();
      resolve();
    };
    child.once("spawn", () =>
      done(() => {
        child.unref();
        notify(
          full ? "Updating veld" : "Updating Veld Desktop",
          full
            ? `Installing ${version} — CLI, daemon and app. The app will reopen.`
            : `Installing ${version} — the app will reopen.`,
        );
        // Not `quitAndInstall`: nothing here goes through Squirrel. A plain quit
        // is what the CLI is waiting for.
        app.quit();
      }),
    );
    child.once("error", (err) =>
      done(() => {
        void dialog.showMessageBox({
          type: "warning",
          message: "The update could not be started.",
          detail: `${String(err?.message ?? err)}\n\nRun \`veld update\` in a terminal instead.`,
          buttons: ["OK"],
        });
      }),
    );
  });
}

/**
 * Tell the user how the last CLI-handed update went, now that there is a window
 * to tell them in.
 *
 * The handoff is one-way by construction — the app quits so its bundle can be
 * replaced — so the CLI leaves a note instead. Read once and deleted: a report
 * still sitting there on the next launch would re-announce a failure the user has
 * already seen and acted on.
 */
async function reportPreviousCliUpdate() {
  const reportPath = updateReportPath();
  /** @type {{version?: string, ok?: boolean, error?: string, log?: string} | null} */
  let report = null;
  try {
    report = JSON.parse(fs.readFileSync(reportPath, "utf8"));
  } catch {
    return; // No note, or an unreadable one. Either way there is nothing to say.
  }
  try {
    fs.rmSync(reportPath, { force: true });
  } catch {
    // Reported once is the intent; a file we cannot delete would repeat it, and
    // that is still better than staying silent about a failed update.
  }
  // A success needs no dialog: the user asked for an update and is looking at
  // it. `app.getVersion()` is the receipt.
  if (!report || report.ok !== false) return;
  // …and neither does a report about some earlier day's handoff. One left behind
  // by a failed update was read a day later by a different, newer install, which
  // duly announced that a version it had never tried to install had failed.
  if (!reportIsFresh({ finishedAt: report.finished_at })) return;

  // What failed decides both sentences. A `veld update` handoff can fail on the
  // *CLI* half — the download, the check, the service restart — and never reach
  // the app at all; telling that user to run `veld desktop update` would move
  // the app and leave the daemon on the release that actually broke. Reports
  // written before `half` existed came only from the app-only command, so
  // treating a missing field as `"app"` is the right reading of an old file.
  const wholeRelease = report.half === "release";
  const buttons = report.log ? ["Show Log", "Close"] : ["Close"];
  const { response } = await dialog.showMessageBox({
    type: "warning",
    message: (wholeRelease
      ? `veld ${report.version ?? ""} was not installed.`
      : `Veld Desktop ${report.version ?? ""} was not installed.`
    ).replace(/\s+/g, " "),
    detail: `${report.error ?? "The veld CLI did not say why."}\n\nYou are still on ${app.getVersion()}. Run \`${wholeRelease ? "veld update" : "veld desktop update"}\` in a terminal to retry.`,
    buttons,
    defaultId: 0,
    cancelId: buttons.length - 1,
  });
  if (report.log && response === 0) shell.showItemInFolder(report.log);
}

async function promptRestart(version) {
  const { response } = await dialog.showMessageBox({
    type: "info",
    message: `Veld Desktop ${version ?? ""} is ready to install.`.replace(
      /\s+/g,
      " ",
    ),
    // "…but their panes are re-opened empty" used to end this sentence, and it
    // has not been true since sessions moved into per-session holder processes:
    // a restored pane reattaches to the same shell and the daemon replays its
    // scrollback (`crates/veld-daemon/src/pty/holder.rs`, and the row on session
    // lifetime in `ARCHITECTURE.md`). Telling the user to finish up first made
    // them schedule around a restart that costs nothing.
    detail:
      "The app restarts to apply it. Terminal sessions survive — they belong to the daemon, not to this window — and their panes reattach to the same shells, with scrollback, when it comes back.",
    buttons: ["Restart Now", "Later"],
    defaultId: 0,
    cancelId: 1,
  });
  if (response !== 0) return;
  // `isSilent: true`, `isForceRunAfter: true` — the user is standing right here.
  // Anything that goes wrong from here surfaces through the `error` listener,
  // which is the only path that reaches it: this call is synchronous and its
  // failures are emitted, not thrown to us.
  installing = true;
  installingVersion = version ?? null;
  // Released first: `AppImageUpdater` spawns the replacement *before* this
  // process quits (`BaseUpdater.quitAndInstall` schedules the quit on
  // `setImmediate`), and the new instance takes the single-instance lock in
  // `main.js` on startup. Holding it here means a slow teardown leaves the user
  // with the successor quitting itself and nothing running — on the one platform
  // that self-installs.
  app.releaseSingleInstanceLock();
  autoUpdater.quitAndInstall(true, true);
}

/**
 * The install failed after the app was already committed to it. On Linux that
 * can mean the running AppImage is gone (electron-updater unlinks it before the
 * replacement is moved into place), so this offers the download rather than only
 * apologising — for some users it is the only way back to a working app.
 */
async function reportInstallFailure(err) {
  const { response } = await dialog.showMessageBox({
    type: "error",
    message: "The update could not be installed.",
    detail: `${String(err?.message ?? err)}\n\nDownload it from the release page instead — on Linux the previous AppImage may already have been removed.`,
    buttons: ["Open Release Page", "Close"],
    defaultId: 0,
    cancelId: 1,
  });
  if (response === 0) await shell.openExternal(releasePageUrl(installingVersion));
}

/**
 * Record the version the daemon reports, and tell the user once per session when
 * it disagrees with this build. Called on every successful reachability check,
 * so it also *clears* the notice when `veld update` catches the daemon up.
 *
 * @param {string | null | undefined} daemonVersion
 */
function noteDaemonVersion(daemonVersion) {
  const skew = versionSkew({
    appVersion: app.getVersion(),
    daemonVersion,
    isPackaged: app.isPackaged,
  });
  const changed = (currentSkew?.daemonVersion ?? null) !== (skew?.daemonVersion ?? null);
  currentSkew = skew;
  if (changed) onSkewChange?.();
  if (!skew || skewReported.has(skew.daemonVersion)) return;
  skewReported.add(skew.daemonVersion);
  // A notification rather than a dialog: nothing is broken, and the app is
  // usable in exactly the state the user just opened it in.
  notify(
    "Veld versions don't match",
    skew.behind === "daemon"
      ? `The veld CLI is ${skew.daemonVersion}, this app is ${skew.appVersion}. Run \`veld update\`.`
      : `The veld CLI is ${skew.daemonVersion}, this app is ${skew.appVersion}. Update Veld Desktop.`,
  );
}

/**
 * The skew as a tray-menu row, or `null` when the two agree. Kept here rather
 * than in the tray so there is one description of the mismatch.
 *
 * @returns {Electron.MenuItemConstructorOptions | null}
 */
function skewMenuItem() {
  if (!currentSkew) return null;
  return currentSkew.behind === "daemon"
    ? {
        label: `veld CLI is ${currentSkew.daemonVersion} — run \`veld update\``,
        enabled: false,
      }
    : {
        label: `Veld Desktop is ${currentSkew.appVersion}, daemon is ${currentSkew.daemonVersion}`,
        click: () => void checkForUpdates({ manual: true }),
      };
}

function notify(title, body) {
  if (!Notification.isSupported()) return;
  new Notification({ title, body }).show();
}

module.exports = {
  checkForUpdates,
  initUpdater,
  noteDaemonVersion,
  quitIfUpdating,
  settingsChanged,
  skewMenuItem,
};
