// Pure decision logic for updating Veld Desktop, and for reporting the version
// skew between the app and the daemon it is talking to.
//
// Electron-free on purpose, the same way `validate.js` is: this is the part with
// branches worth testing (which platforms can install an update in place, which
// direction a version mismatch points), and the runner has no Chromium.

const GITHUB_REPO = "prosperity-solutions/veld";

/**
 * How this build is allowed to apply an update.
 *
 * - `"off"` — an unpackaged run (`npm start`): there is no bundle to replace,
 *   and electron-updater refuses outright.
 * - `"install"` — the update can be downloaded and applied by the app itself.
 *   Today that is the Linux AppImage only: it is a single file the runtime can
 *   swap, and `APPIMAGE` in the environment is how the running process knows it
 *   *is* one (a .deb install has no such variable, and nothing in it a process
 *   may replace without the package manager).
 * - `"download"` — check and tell the user, but hand the install to them. macOS
 *   is here because Squirrel.Mac verifies that the replacement carries the same
 *   code signature as the running app, and veld has no Developer ID yet (issue
 *   #167 §10); the .deb is here because its files belong to dpkg.
 *
 * The macOS half flips to `"install"` by deleting one line, once signing lands.
 *
 * @param {{platform: string, isPackaged: boolean, env?: Record<string, string | undefined>}} ctx
 * @returns {"off" | "install" | "download"}
 */
function updateMode({ platform, isPackaged, env = {} }) {
  if (!isPackaged) return "off";
  // Not signed yet → Squirrel.Mac would reject the swap after the download.
  if (platform === "darwin") return "download";
  if (platform === "linux") return env.APPIMAGE ? "install" : "download";
  return "download";
}

/**
 * Compare two `major.minor.patch` strings. Missing or non-numeric components
 * count as 0, matching `veld_core::setup::is_newer` — the CLI's own comparison,
 * against the same GitHub releases, so "is there an update" cannot answer
 * differently in the two places a user might ask it.
 *
 * @returns {number} negative if `a` < `b`, 0 if equal, positive if `a` > `b`
 */
function compareVersions(a, b) {
  const parse = (v) =>
    String(v ?? "")
      .replace(/^v/, "")
      .split(".")
      .slice(0, 3)
      .map((part) => {
        const n = Number.parseInt(part, 10);
        return Number.isNaN(n) ? 0 : n;
      });
  const left = parse(a);
  const right = parse(b);
  for (let i = 0; i < 3; i++) {
    const diff = (left[i] ?? 0) - (right[i] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

/**
 * Whether the app and the daemon are the mismatched halves of one release.
 *
 * They ship from a single tag with a single version (see
 * `desktop/ARCHITECTURE.md` → "Packaging"), so a difference means one of the two
 * updated and the other did not — and which one decides what the user has to do:
 * the app updates itself, the daemon updates through `veld update`. The UI the
 * shell renders comes from the *daemon*, so an old daemon is the one that
 * actually loses features; an old shell only misses the IPC a newer UI expects,
 * which the UI already feature-detects.
 *
 * Returns `null` when they agree, when either version is unknown, or for an
 * unpackaged run — a dev build's version is `0.0.0` and would report skew
 * against every daemon.
 *
 * @param {{appVersion: string, daemonVersion: string | null | undefined, isPackaged: boolean}} ctx
 * @returns {{behind: "daemon" | "app", appVersion: string, daemonVersion: string} | null}
 */
function versionSkew({ appVersion, daemonVersion, isPackaged }) {
  if (!isPackaged || !appVersion || !daemonVersion) return null;
  const diff = compareVersions(appVersion, daemonVersion);
  if (diff === 0) return null;
  return {
    behind: diff > 0 ? "daemon" : "app",
    appVersion,
    daemonVersion,
  };
}

/** The page a user lands on to pick the right artifact for their machine. */
function releasePageUrl(version) {
  const tag = version ? `tag/v${String(version).replace(/^v/, "")}` : "latest";
  return `https://github.com/${GITHUB_REPO}/releases/${tag}`;
}

module.exports = {
  GITHUB_REPO,
  compareVersions,
  releasePageUrl,
  updateMode,
  versionSkew,
};
