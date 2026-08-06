// Pure decision logic for updating Veld Desktop, and for reporting the version
// skew between the app and the daemon it is talking to.
//
// Electron-free on purpose, the same way `validate.js` is: this is the part with
// branches worth testing (which platforms can install an update in place, which
// direction a version mismatch points), and the runner has no Chromium.

const GITHUB_REPO = "prosperity-solutions/veld";

/**
 * Whether the macOS build carries a Developer ID signature (issue #167 §10).
 *
 * A constant rather than a branch to delete, because "delete this line" was the
 * wrong instruction: `updateMode`'s catch-all also returns `"download"`, so
 * removing the darwin case changes nothing and the existing test still passes —
 * a contributor doing exactly what the comment said would ship no behaviour
 * change and believe otherwise. Flipping this to `true` (and packaging with a
 * real identity, and notarizing) is the whole switch, and both sides of it are
 * tested.
 */
const MACOS_SIGNED = false;

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
 * - `"cli"` — hand the update to the veld CLI, which quits this app, replaces the
 *   bundle and reopens it. macOS only, and only when the CLI is actually present.
 *   It works where the app cannot replace *itself*: Squirrel.Mac accepts only a
 *   replacement carrying the running app's signature, which an ad-hoc build does
 *   not have — while the CLI installs the same release the installer does, with
 *   curl, which never sets `com.apple.quarantine`, so Gatekeeper is not consulted
 *   at all. It also keeps the app and the CLI on one version, which is the shape
 *   the release already promises.
 * - `"download"` — check and tell the user, but hand the install to them. macOS
 *   lands here only with no CLI to delegate to, because Squirrel.Mac verifies that
 *   the replacement carries the same code signature as the running app and veld
 *   has no Developer ID yet (issue #167 §10); the .deb is here because its files
 *   belong to dpkg.
 *
 * The macOS *self*-install half flips with `MACOS_SIGNED` above, once signing
 * lands. `"cli"` outranks it either way: same-version-as-the-CLI is worth more
 * than Squirrel's delta downloads, and it is the one route that works whether or
 * not the build is signed.
 *
 * @param {{platform: string, isPackaged: boolean, env?: Record<string, string | undefined>, macSigned?: boolean, cli?: string | null}} ctx
 * @returns {"off" | "install" | "download" | "cli"}
 */
function updateMode({
  platform,
  isPackaged,
  env = {},
  macSigned = MACOS_SIGNED,
  cli = null,
}) {
  if (!isPackaged) return "off";
  if (platform === "darwin") {
    if (cli) return "cli";
    // Unsigned → Squirrel.Mac rejects the swap after the download, so there is
    // nothing to gain by starting one.
    return macSigned ? "install" : "download";
  }
  if (platform === "linux") return env.APPIMAGE ? "install" : "download";
  return "download";
}

/**
 * Why this build cannot apply an update itself — the sentence a `"download"`
 * mode has to justify itself with. Split out from `updateMode` because the two
 * platforms are download-only for unrelated reasons and one string covering both
 * is a falsehood on whichever platform it was not written for.
 *
 * @param {{platform: string}} ctx
 * @returns {string}
 */
function downloadOnlyReason({ platform }) {
  if (platform === "darwin") {
    return "Veld Desktop isn't code-signed yet, so macOS won't let it replace itself.";
  }
  if (platform === "linux") {
    return "This is a .deb install, so its files belong to your package manager.";
  }
  return "This build can't replace itself.";
}

/**
 * Compare two `major.minor.patch` strings. Missing or non-numeric components
 * count as 0, following `veld_core::setup::is_newer` — the CLI's own comparison,
 * against the same GitHub releases, so "is there an update" answers the same way
 * in the two places a user might ask it.
 *
 * The two agree on every version either side publishes, and only there: on a
 * component like `5-rc`, `parseInt` takes the leading 5 where Rust's `parse`
 * rejects the whole component and falls back to 0. Neither side tags a
 * prerelease, so this stays a difference in the parsers rather than in the
 * answers — worth knowing before anyone adds one.
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
  if (!isPackaged) return null;
  // Strings only. `daemonVersion` comes off the wire from whatever answers
  // `127.0.0.1:19899/api/health`, and a non-string sails through
  // `compareVersions` (which coerces) into a `Set` key and a notification body:
  // an object key is never equal to the next poll's, so the once-per-session
  // guard stops guarding and the toast repeats every minute. A daemon that
  // cannot state its version has nothing to say here anyway.
  if (typeof appVersion !== "string" || typeof daemonVersion !== "string") {
    return null;
  }
  if (!appVersion || !daemonVersion) return null;
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
  downloadOnlyReason,
  releasePageUrl,
  updateMode,
  versionSkew,
};
