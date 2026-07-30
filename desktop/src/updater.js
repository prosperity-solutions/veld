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

const { Notification, app, dialog, shell } = require("electron");
const { autoUpdater } = require("electron-updater");

const {
  downloadOnlyReason,
  releasePageUrl,
  updateMode,
  versionSkew,
} = require("./updatePolicy");

/** Let the window come up first — an update prompt is never the reason someone opened the app. */
const FIRST_CHECK_DELAY_MS = 15_000;
const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;

/** @type {"off" | "install" | "download"} */
let mode = "off";
/** Versions already offered this session, so a periodic check can't re-ask. */
const offered = new Set();
/** Daemon versions already reported as skewed, same reason. */
const skewReported = new Set();
let checking = false;
/** Set across `quitAndInstall` — see the error listener in `initUpdater`. */
let installing = false;
/** Version being installed, for the failure dialog's download link. */
let installingVersion = null;
/** @type {{behind: "daemon" | "app", appVersion: string, daemonVersion: string} | null} */
let currentSkew = null;
/** @type {(() => void) | null} */
let onSkewChange = null;

/**
 * Wire up the updater and start the background schedule.
 *
 * @param {{onSkewChange?: () => void}} [opts] called when the daemon-skew notice
 *   appears or clears, so the tray menu can re-render without polling for it.
 */
function initUpdater(opts = {}) {
  onSkewChange = opts.onSkewChange ?? null;
  mode = updateMode({
    platform: process.platform,
    isPackaged: app.isPackaged,
    env: process.env,
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
    void reportInstallFailure(err);
  });

  autoUpdater.on("update-downloaded", (info) => {
    void promptRestart(info?.version);
  });

  if (mode === "off") return;

  setTimeout(() => void checkForUpdates({ manual: false }), FIRST_CHECK_DELAY_MS);
  const timer = setInterval(
    () => void checkForUpdates({ manual: false }),
    CHECK_INTERVAL_MS,
  );
  // Nothing here should hold the process alive on its own.
  timer.unref?.();
}

/**
 * @param {{manual: boolean}} opts `manual: true` is a user-initiated check, which
 *   reports outcomes an automatic one swallows.
 */
async function checkForUpdates({ manual }) {
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
  if (checking) return;
  checking = true;
  try {
    const result = await autoUpdater.checkForUpdates();
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
    offered.add(version);
    await offerUpdate(version);
  } catch (err) {
    if (manual) {
      await dialog.showMessageBox({
        type: "warning",
        message: "Could not check for updates.",
        detail: String(err?.message ?? err),
        buttons: ["OK"],
      });
    }
  } finally {
    checking = false;
  }
}

async function offerUpdate(version) {
  const canInstall = mode === "install";
  const { response } = await dialog.showMessageBox({
    type: "info",
    message: `Veld Desktop ${version} is available.`,
    detail: canInstall
      ? `You're on ${app.getVersion()}. Downloading takes a moment; the app restarts to apply it.`
      : `You're on ${app.getVersion()}. ${downloadOnlyReason({ platform: process.platform })} The release page has the download.\n\nUpdate the veld CLI separately with \`veld update\`.`,
    buttons: canInstall
      ? ["Download and Install", "Later"]
      : ["Open Release Page", "Later"],
    defaultId: 0,
    cancelId: 1,
  });
  if (response !== 0) return;

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

async function promptRestart(version) {
  const { response } = await dialog.showMessageBox({
    type: "info",
    message: `Veld Desktop ${version ?? ""} is ready to install.`.replace(
      /\s+/g,
      " ",
    ),
    detail:
      "The app restarts to apply it. Terminal sessions survive — they belong to the daemon, not to this window — but their panes are re-opened empty, so finish anything mid-flight first.",
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
  skewMenuItem,
};
