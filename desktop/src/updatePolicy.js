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

/**
 * How long a handoff report stays meaningful.
 *
 * The whole exchange is seconds long: the CLI writes the outcome and the app is
 * already relaunching. Fifteen minutes is far longer than that and still short
 * enough that nothing ancient survives.
 */
const REPORT_MAX_AGE_MS = 15 * 60 * 1000;

/**
 * Whether a `desktop-update.json` describes the handoff this launch just came
 * back from, rather than one from some earlier day.
 *
 * There was no such check, and the failure it allows is not hypothetical: a
 * report left behind by a failed update sat in `~/.veld` for a day, and the next
 * time the app started — a *different* install, of a newer version — it read the
 * file and announced that "Veld Desktop 99.0.0 was not installed". Everything
 * downstream of it was working correctly; the report simply had no expiry.
 *
 * Missing or unparseable timestamps count as stale. A report that cannot say
 * when it was written cannot claim to be about this launch, and staying quiet is
 * the cheaper mistake — the alternative is telling someone an update failed when
 * nothing of the sort just happened.
 *
 * Clock skew is tolerated in both directions by the same margin: a timestamp
 * slightly in the future is a machine whose clock moved, not a lie.
 *
 * @param {{finishedAt?: string | null, now?: number, maxAgeMs?: number}} ctx
 * @returns {boolean}
 */
function reportIsFresh({ finishedAt, now = Date.now(), maxAgeMs = REPORT_MAX_AGE_MS }) {
  if (typeof finishedAt !== "string" || !finishedAt) return false;
  const written = Date.parse(finishedAt);
  if (Number.isNaN(written)) return false;
  return Math.abs(now - written) <= maxAgeMs;
}

/**
 * How long a lock holder may sit in one phase before it is written off.
 *
 * Mirrors `PHASE_TIMEOUT` in `crates/veld-core/src/update_lock.rs`, and the
 * duplication is deliberate rather than a shared constant: this file is
 * dependency-free by design, and the only thing the two copies must agree on is
 * "roughly half an hour". Reading a stale lock as live for a few minutes longer
 * than the CLI would costs a dialog, not correctness — `acquire` on the Rust side
 * remains the only thing that ever *acts* on staleness.
 */
const UPDATE_PHASE_TIMEOUT_MS = 30 * 60 * 1000;

/**
 * Whether `~/.veld/update.lock/state.json` describes an update that is really
 * running.
 *
 * Two independent staleness conditions, same as the CLI's: the holder is gone,
 * or it has not changed phase in {@link UPDATE_PHASE_TIMEOUT_MS}. Both are needed
 * — a liveness check cannot see a run wedged at a `sudo` prompt, and a timeout
 * cannot tell a crash from a slow success.
 *
 * `pidAlive` is injected so this stays testable without spawning processes; the
 * caller passes a `process.kill(pid, 0)` probe.
 *
 * An unparseable or shapeless state file reads as "no update". The app quits
 * itself on the strength of this answer, so the burden of proof is on the file:
 * garbage in `~/.veld` must never make Veld Desktop unopenable.
 *
 * @param {{state: unknown, now?: number, pidAlive?: (pid: number) => boolean}} ctx
 * @returns {{pid: number, phase: string, version: string | null, origin: string} | null}
 */
function updateInProgress({ state, now = Date.now(), pidAlive = () => true }) {
  if (!state || typeof state !== "object") return null;
  const { pid, phase, phase_at: phaseAt, version, origin } = /** @type {any} */ (state);
  if (typeof pid !== "number" || !Number.isInteger(pid) || pid <= 0) return null;
  if (!pidAlive(pid)) return null;
  const moved = Date.parse(typeof phaseAt === "string" ? phaseAt : "");
  // A missing or unparseable timestamp cannot vouch for a live update. Same
  // direction as `reportIsFresh`: silence is the cheaper mistake.
  if (Number.isNaN(moved)) return null;
  // One-sided, unlike `reportIsFresh`: a `phase_at` in the future is a clock that
  // moved, not evidence of abandonment, so only the past is checked.
  if (now - moved > UPDATE_PHASE_TIMEOUT_MS) return null;
  return {
    pid,
    phase: typeof phase === "string" ? phase : "starting",
    version: typeof version === "string" ? version : null,
    origin: typeof origin === "string" ? origin : "cli",
  };
}

/**
 * What to put in the "an update is running" dialog, for a given phase.
 *
 * Kept beside the phase names rather than inlined at the dialog, because these
 * strings are the app's half of a vocabulary the CLI defines — an unknown phase
 * (an older app, a newer CLI) has to degrade to something true rather than to
 * `undefined`.
 *
 * @param {string} phase
 * @returns {string}
 */
function updatePhaseLabel(phase) {
  switch (phase) {
    case "starting":
      return "starting up";
    case "waiting-for-app":
      return "waiting for Veld Desktop to quit";
    case "checking":
      return "checking which release to install";
    case "installing":
      return "downloading and installing";
    case "restarting-services":
      return "restarting the daemon and helper";
    case "updating-app":
      return "installing Veld Desktop";
    case "finishing":
      return "finishing up";
    default:
      return "in progress";
  }
}

/** The page a user lands on to pick the right artifact for their machine. */
function releasePageUrl(version) {
  const tag = version ? `tag/v${String(version).replace(/^v/, "")}` : "latest";
  return `https://github.com/${GITHUB_REPO}/releases/${tag}`;
}

/**
 * Where to look for the veld CLI, in the order the app is willing to trust.
 *
 * A GUI app has no usable PATH — a launchd-started one gets a bare service PATH
 * — so `which veld` is not a question that can be asked. These are the
 * directories `install.sh` writes to, in the order it prefers them, so the app
 * resolves the same binary the installer last wrote.
 *
 * **This order is not a security boundary, and an earlier version of this
 * comment claimed it was.** The claim was that root-owned prefixes are probed
 * before the user-writable one — but on Apple Silicon `/opt/homebrew/bin` is
 * `drwxrwxr-x <user>:admin`, i.e. writable by the same user as `~/.local/bin`,
 * and it is ranked above it. More to the point, anything that can write a file
 * into *any* of these directories can already replace the real veld binary, so
 * no ordering of them buys a defence. What the order actually buys is agreement
 * with `install.sh`: prefer a system prefix, fall back to `$HOME`. Do not
 * reintroduce a security argument here without changing the mechanism.
 *
 * @param {{home: string}} ctx
 * @returns {string[]}
 */
function cliCandidatePaths({ home }) {
  return [
    "/usr/local/bin/veld",
    "/opt/homebrew/bin/veld",
    `${home}/.local/bin/veld`,
  ];
}

/**
 * Whether `veld --version` output came from the veld CLI.
 *
 * Being executable and being named `veld` is not the same as being veld. Be
 * precise about what this buys, because the obvious reading is wrong: the check
 * is performed *by running the candidate*, so it cannot stop a bogus binary from
 * executing — by the time this sees any output, it has already run. What it
 * stops is the second, worse execution: without it, a wrong binary would be
 * re-spawned **detached**, unbounded, with the app quitting behind it. With it,
 * a wrong binary gets one 2-second, `PATH`-restricted, output-inspected run and
 * is then discarded.
 *
 * The CLI prints `veld <semver>` (clap's `--version`), so that is what this
 * accepts — and nothing that merely mentions the word.
 *
 * @param {string | null | undefined} output
 * @returns {boolean}
 */
function looksLikeVeldCli(output) {
  if (typeof output !== "string") return false;
  return /^veld\s+v?\d+\.\d+\.\d+/i.test(output.trim());
}

/**
 * The capability the CLI advertises when `veld update` can carry the whole
 * release — both halves — on the app's behalf.
 */
const FULL_UPDATE_HANDOFF = "full-update-handoff";

/**
 * `veld update` understands `--console` — i.e. it can re-run itself in a terminal
 * window so the user can watch the update and `sudo` has somewhere to prompt.
 *
 * Separate from {@link FULL_UPDATE_HANDOFF} because the two can genuinely differ:
 * `veld desktop update` moves the app half alone, so an app on the new release
 * can be driving a CLI on the old one, and that CLI advertises the full handoff
 * (it has always had those flags) while rejecting `--console` outright.
 */
const CONSOLE_HANDOFF = "console-handoff";

/**
 * What the CLI said it can do, from `veld desktop status --json`.
 *
 * Defensive to the point of pedantry because the parse happens on the path that
 * decides *which command to spawn with the app about to quit*: unparseable
 * output, a missing key, a `capabilities` that is a string rather than an array,
 * or non-string members all resolve to "advertises nothing", which selects the
 * older command that every shipped CLI understands. The failure mode being
 * avoided is spawning `veld update --wait-pid` at a CLI that rejects the flag,
 * exits 2, and leaves the user with no window and no update.
 *
 * @param {string | null | undefined} stdout
 * @returns {string[]}
 */
function capabilitiesFrom(stdout) {
  if (typeof stdout !== "string") return [];
  let parsed;
  try {
    parsed = JSON.parse(stdout);
  } catch {
    return [];
  }
  if (!parsed || !Array.isArray(parsed.capabilities)) return [];
  return parsed.capabilities.filter((c) => typeof c === "string");
}

/**
 * The command that hands this app's update to the CLI.
 *
 * Two shapes, and which one is chosen is a compatibility question rather than a
 * preference:
 *
 * - `veld update …` when the CLI advertises `full-update-handoff`. This moves the
 *   CLI, the daemon, the helper *and* the app from one release, which is what the
 *   user asked for when they clicked a button offering them a new veld. The
 *   version travels as `--target-version`, spelled differently from the older
 *   route's `--version` because it means something different — "install this
 *   release" rather than "install this app build" — and because an older CLI must
 *   reject it outright rather than half-understand it.
 * - `veld desktop update --version …` otherwise. The app half only — the older
 *   CLI's whole vocabulary — and `--version` is required there for exactly the
 *   loop the newer path cannot have: an older CLI would otherwise reinstall its
 *   *own* version, relaunch, be offered the newer one again, and never converge.
 *
 * `--app-path` and `--wait-pid` are on both: which bundle to replace, and the
 * process that must be gone before anything touches it.
 *
 * @param {{capabilities?: string[], version: string, pid: number, execPath: string}} ctx
 * @returns {{args: string[], full: boolean}}
 */
function handoffCommand({ capabilities = [], version, pid, execPath }) {
  const full = capabilities.includes(FULL_UPDATE_HANDOFF);
  // A **separate** capability from `full`, and the separation is load-bearing.
  // `veld desktop update` moves the app half alone, so a new app can be driving
  // an old CLI — one that has always had `--wait-pid`/`--relaunch` and therefore
  // advertises `full-update-handoff`, but whose clap rejects `--console` with a
  // usage error and exit 2. The app has quit by then and no report is written,
  // so the user would reopen on the old version having been told nothing.
  const consoleHandoff = capabilities.includes(CONSOLE_HANDOFF);
  const args = full
    ? [
        "update",
        // Run the update in a terminal window rather than here. Two things the
        // detached-child route could not do: show the user 1–4 minutes of
        // progress after this app has quit, and give `sudo` a terminal to ask
        // for the password in — a privileged install restarts a root helper, and
        // a child with no controlling terminal only ever gets `sudo -n`. The CLI
        // falls back to running headless when no terminal can be opened, so this
        // never makes an update fail that would otherwise have worked.
        ...(consoleHandoff ? ["--console"] : []),
        // The release the user was just offered, from the feed that offered it.
        // Without this the CLI asks `api.github.com/…/releases/latest` — a
        // second source, rate-limited per IP and briefly out of step with the
        // feed after a release — so a handoff could abort on a 403 or install
        // nothing and re-offer the same version forever.
        "--target-version",
        version,
        "--wait-pid",
        String(pid),
        "--relaunch",
        "--app-path",
        execPath,
      ]
    : [
        "desktop",
        "update",
        "--version",
        version,
        "--wait-pid",
        String(pid),
        "--relaunch",
        "--app-path",
        execPath,
      ];
  return { args, full };
}

/**
 * The label on the button that does the thing.
 *
 * Kept beside `updateMode` rather than in the dialog, because it is the same
 * decision wearing a different hat: each mode can do exactly one thing, and the
 * label is a promise about which. "Quit and Update veld" is reserved for the one
 * route that moves the CLI too — everything else says the app.
 *
 * @param {{viaCli: boolean, canInstall: boolean, full: boolean}} ctx
 * @returns {string}
 */
function primaryAction({ viaCli, canInstall, full }) {
  if (viaCli) return full ? "Quit and Update veld" : "Quit and Update";
  if (canInstall) return "Download and Install";
  return "Open Release Page";
}

module.exports = {
  CONSOLE_HANDOFF,
  FULL_UPDATE_HANDOFF,
  GITHUB_REPO,
  REPORT_MAX_AGE_MS,
  UPDATE_PHASE_TIMEOUT_MS,
  capabilitiesFrom,
  cliCandidatePaths,
  compareVersions,
  downloadOnlyReason,
  handoffCommand,
  looksLikeVeldCli,
  primaryAction,
  releasePageUrl,
  reportIsFresh,
  updateInProgress,
  updateMode,
  updatePhaseLabel,
  versionSkew,
};
