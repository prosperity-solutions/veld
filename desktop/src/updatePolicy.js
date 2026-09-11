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
 * How eagerly this machine wants to be told about a new release.
 *
 * One setting, `desktop.updateFrequency`, with three answers — and the knobs
 * below exist because "how often" turned out to be three separate questions that
 * a single interval answered badly. The app used to check every six hours and
 * offer whatever it found, which is why the complaint was never about the
 * checking: a release train that ships several times a day produced several
 * dialogs a day, each one correct and each one an interruption.
 *
 * - `checkIntervalMs` — how often the network check runs. Silent either way;
 *   this is the only knob that costs anything, and it is the *least* important
 *   of the three.
 * - `minReleaseAgeMs` — how long a release has to have existed before it is
 *   worth interrupting somebody for. The point of waiting is that the release
 *   that follows a bad one lands within hours, so a tier that waits skips the
 *   dialog for both.
 * - `versionsAheadOverride` — how many *seen* releases is enough to ask anyway,
 *   when the age gate has not opened yet. This is what stops a quiet tier from
 *   sitting out an entire week of releases — and on a train that ships several
 *   times a day it is the gate that actually fires, because the newest release
 *   is never old enough to clear `minReleaseAgeMs`. "Seen" is the honest word:
 *   see {@link versionsAhead} for why the true release count is not available.
 * - `minPromptGapMs` — the floor between two prompts, whatever else is true.
 *   The one knob a user can predict from the label: "at most once a day".
 *
 * Default is {@link DEFAULT_UPDATE_FREQUENCY}, and it is deliberately quieter
 * than the behaviour it replaces on every axis that a person can perceive: the
 * same six-hour check, but a release has to be a day and a half old (or the
 * fourth one this app has seen) before it produces a dialog, and never more than
 * one dialog a day.
 *
 * @type {Record<string, {checkIntervalMs: number, minReleaseAgeMs: number, versionsAheadOverride: number, minPromptGapMs: number}>}
 */
const UPDATE_TIERS = {
  // Every release, as soon as the check finds it. For someone who wants the
  // newest build — and accepts that "newest" and "settled" are different things.
  eager: {
    checkIntervalMs: 60 * 60 * 1000,
    minReleaseAgeMs: 0,
    versionsAheadOverride: 1,
    minPromptGapMs: 0,
  },
  // The default. At most one prompt a day, for a release that has had a day and
  // a half to be superseded — or for a fourth release that has piled up behind
  // the gate, which is the case the age rule alone handles badly.
  balanced: {
    checkIntervalMs: 6 * 60 * 60 * 1000,
    minReleaseAgeMs: 36 * 60 * 60 * 1000,
    versionsAheadOverride: 4,
    minPromptGapMs: 24 * 60 * 60 * 1000,
  },
  // For someone happy to run a version that is a few days old. Two days between
  // prompts, three days of ripening, and eight seen releases is the only thing
  // that shortcuts the *age* gate — nothing shortcuts the prompt gap.
  relaxed: {
    checkIntervalMs: 12 * 60 * 60 * 1000,
    minReleaseAgeMs: 72 * 60 * 60 * 1000,
    versionsAheadOverride: 8,
    minPromptGapMs: 48 * 60 * 60 * 1000,
  },
};

/** The tier a machine that has never touched the setting is on. */
const DEFAULT_UPDATE_FREQUENCY = "balanced";

/**
 * How long "Later" lasts.
 *
 * The button says *Later*, so a decline that never expired would make the label
 * a lie: the feed only ever names the newest release, so on a quiet week an
 * automatic check would never raise that version again. A week is long enough
 * that declining actually buys quiet — the whole point — and short enough that
 * somebody who meant "not today" is asked again eventually rather than never.
 *
 * Independent of the tier. The tier governs how often you are asked about
 * releases in general; this governs one release you have already answered about,
 * and a `relaxed` user who declined has not asked to be *never* told again.
 */
const DECLINE_EXPIRY_MS = 7 * 24 * 60 * 60 * 1000;

/**
 * The tier named by a stored setting value.
 *
 * Anything unrecognised is the default, on purpose and in both directions: a
 * *newer* daemon could offer a tier this app has never heard of (the settings
 * document is shared across clients and outlives any one app build), and a
 * garbled value must not leave the updater with no schedule at all. The Rust
 * side validates against the same three names, so this path is reached by
 * version skew rather than by a user typo.
 *
 * @param {unknown} value
 */
function updateTier(value) {
  return isTierName(value) ? UPDATE_TIERS[value] : UPDATE_TIERS[DEFAULT_UPDATE_FREQUENCY];
}

/**
 * Whether a stored value names a tier this build has.
 *
 * `Object.hasOwn` rather than `in` or a truthy lookup, and it is not pedantry:
 * `UPDATE_TIERS` is an object literal, so `"constructor"` and `"toString"` are
 * `in` it and resolve to functions off `Object.prototype`. A stored value of
 * `"constructor"` would then be accepted as a tier and read as
 * `tier.checkIntervalMs === undefined`, which `setTimeout` treats as zero — a
 * check every tick, from a string nobody validated on the way in.
 *
 * @param {unknown} value
 * @returns {value is keyof typeof UPDATE_TIERS}
 */
function isTierName(value) {
  return typeof value === "string" && Object.hasOwn(UPDATE_TIERS, value);
}

/**
 * The `desktop.updateFrequency` value in a `GET /api/settings` body.
 *
 * Shaped like `trayVisibility.js`'s `menuBarIconFrom` and for the same reason:
 * the fetch can fail, the key can be absent on an older daemon, and the value
 * can be any JSON at all. Only a string that names a tier this build knows moves
 * the answer off `fallback` — which keeps a daemon that has never heard of the
 * key from silently re-tiering the app.
 *
 * @param {unknown} body
 * @param {string} fallback
 * @returns {string}
 */
function updateFrequencyFrom(body, fallback = DEFAULT_UPDATE_FREQUENCY) {
  const value = /** @type {any} */ (body)?.settings?.["desktop.updateFrequency"];
  return isTierName(value) ? value : fallback;
}

/**
 * How long the newest release has been available, in ms.
 *
 * Two sources, and the older answer wins. `releaseDate` comes off the update
 * feed and is the truth when it is there; `firstSeenAt` is when *this* app first
 * recorded the version and is the fallback for a feed that omits the field or
 * writes something unparseable. Taking the older of the two is what makes the
 * age gate survive a laptop that was shut for a week: without `releaseDate`,
 * reopening the app would restart the ripening clock on a release that has been
 * out for days, and the user would be asked to wait all over again.
 *
 * A `releaseDate` in the future is a publisher's clock, not a release from
 * tomorrow, so it is ignored rather than producing a negative age.
 *
 * @param {{releaseDate?: string | null, firstSeenAt?: number | null, now?: number}} ctx
 * @returns {number}
 */
function releaseAgeMs({ releaseDate, firstSeenAt, now = Date.now() }) {
  const candidates = [];
  const published = Date.parse(typeof releaseDate === "string" ? releaseDate : "");
  if (!Number.isNaN(published) && published <= now) candidates.push(published);
  if (typeof firstSeenAt === "number" && Number.isFinite(firstSeenAt) && firstSeenAt <= now) {
    candidates.push(firstSeenAt);
  }
  if (candidates.length === 0) return 0;
  return now - Math.min(...candidates);
}

/**
 * How many releases newer than the installed one this app has actually observed.
 *
 * A **lower bound**, and deliberately not more than that. The feed names only the
 * newest release, so the true count is not available without a second request to
 * a second, rate-limited source — the one the handoff already goes out of its way
 * not to ask (see `handoffCommand`'s `--target-version`). What is available for
 * free is the set of versions this app has seen go past on its own checks, which
 * is exactly the signal `versionsAheadOverride` wants: it answers "have releases
 * been piling up while I stayed quiet", not "how many exist".
 *
 * Versions at or below the running one are ignored rather than trusted, so a
 * downgrade or a re-install cannot inflate the count.
 *
 * @param {{seen: Record<string, number> | null | undefined, currentVersion: string}} ctx
 * @returns {number}
 */
function versionsAhead({ seen, currentVersion }) {
  if (!seen || typeof seen !== "object") return 0;
  return Object.keys(seen).filter((v) => compareVersions(v, currentVersion) > 0).length;
}

/**
 * Whether a decline of this exact version is still in force.
 *
 * Clock-tolerant in both directions, like {@link reportIsFresh} and for the same
 * reason: a `declinedAt` in the future is a machine whose clock moved, and
 * reading it as "declined for the next thirty years" would silence the automatic
 * channel for that version permanently — the failure the expiry exists to
 * prevent, arrived by another road.
 *
 * @param {{declinedAt?: number | null, now?: number, maxAgeMs?: number}} ctx
 * @returns {boolean}
 */
function declineHolds({ declinedAt, now = Date.now(), maxAgeMs = DECLINE_EXPIRY_MS }) {
  if (typeof declinedAt !== "number" || !Number.isFinite(declinedAt)) return false;
  return Math.abs(now - declinedAt) <= maxAgeMs;
}

/**
 * Whether an available release is worth a dialog right now.
 *
 * The whole point of the tiers, in one place and with no I/O, so the awkward
 * combinations are tested rather than reasoned about. Order matters: a manual
 * check answers `true` before anything else is considered, because a person who
 * clicked *Check for Updates…* is owed an answer regardless of how quiet their
 * tier is, and a version declined within the last {@link DECLINE_EXPIRY_MS} is
 * silent regardless of how eager it is.
 *
 * `versionsAhead` overrides only the **age** gate, never `minPromptGapMs`. A
 * burst of releases is a reason to stop waiting for the current one to settle;
 * it is not a reason to prompt twice in an hour, which is the behaviour being
 * fixed.
 *
 * @param {{
 *   tier: {minReleaseAgeMs: number, versionsAheadOverride: number, minPromptGapMs: number},
 *   ageMs: number,
 *   ahead: number,
 *   lastPromptedAt?: number | null,
 *   declinedAt?: number | null,
 *   manual?: boolean,
 *   now?: number,
 * }} ctx
 * @returns {{offer: boolean, reason: "manual" | "declined" | "too-soon" | "ripening" | "aged" | "piled-up"}}
 */
function shouldOfferUpdate({
  tier,
  ageMs,
  ahead,
  lastPromptedAt = null,
  declinedAt = null,
  manual = false,
  now = Date.now(),
}) {
  if (manual) return { offer: true, reason: "manual" };
  if (declineHolds({ declinedAt, now })) return { offer: false, reason: "declined" };
  // One-sided, like `updateInProgress`'s phase check: a `lastPromptedAt` in the
  // future is a clock that moved, and reading it as "the gap has not elapsed"
  // would silence the app until the timestamp catches up — days, on a machine
  // whose clock jumped. Treat it as "no recent prompt" instead.
  if (
    typeof lastPromptedAt === "number" &&
    Number.isFinite(lastPromptedAt) &&
    lastPromptedAt <= now &&
    now - lastPromptedAt < tier.minPromptGapMs
  ) {
    return { offer: false, reason: "too-soon" };
  }
  if (ahead >= tier.versionsAheadOverride) return { offer: true, reason: "piled-up" };
  if (ageMs < tier.minReleaseAgeMs) return { offer: false, reason: "ripening" };
  return { offer: true, reason: "aged" };
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

/**
 * The nudge state, with everything the running version has outgrown removed.
 *
 * Read on every launch and written after every check, so without this the `seen`
 * map is append-only for the life of an install — a release train that ships
 * daily would leave hundreds of dead keys in a file whose only job is to answer
 * two questions about the *current* version. Dropping versions at or below the
 * installed one is also what keeps `versionsAhead` honest after an update: the
 * four releases that finally triggered the prompt must not still be counted
 * against the release they installed.
 *
 * Shape-checked rather than trusted: this file lives in `userData` where anything
 * can edit it, and a malformed one must degrade to "nothing known" rather than
 * to an updater that throws on every check.
 *
 * @param {unknown} state
 * @param {string} currentVersion
 * @returns {{lastPromptedAt: number | null, seen: Record<string, number>, declined: Record<string, number>}}
 */
function pruneUpdateState(state, currentVersion) {
  const raw = state && typeof state === "object" ? /** @type {any} */ (state) : {};
  const ahead = (v) => typeof v === "string" && compareVersions(v, currentVersion) > 0;
  /** @type {Record<string, number>} */
  const seen = {};
  if (raw.seen && typeof raw.seen === "object") {
    for (const [version, at] of Object.entries(raw.seen)) {
      if (ahead(version) && typeof at === "number" && Number.isFinite(at)) seen[version] = at;
    }
  }
  const lastPromptedAt =
    typeof raw.lastPromptedAt === "number" && Number.isFinite(raw.lastPromptedAt)
      ? raw.lastPromptedAt
      : null;
  /** @type {Record<string, number>} */
  const declined = {};
  if (raw.declined && typeof raw.declined === "object" && !Array.isArray(raw.declined)) {
    for (const [version, at] of Object.entries(raw.declined)) {
      if (ahead(version) && typeof at === "number" && Number.isFinite(at)) declined[version] = at;
    }
  }
  return { lastPromptedAt, seen, declined };
}

module.exports = {
  CONSOLE_HANDOFF,
  DECLINE_EXPIRY_MS,
  DEFAULT_UPDATE_FREQUENCY,
  FULL_UPDATE_HANDOFF,
  GITHUB_REPO,
  REPORT_MAX_AGE_MS,
  UPDATE_PHASE_TIMEOUT_MS,
  UPDATE_TIERS,
  capabilitiesFrom,
  cliCandidatePaths,
  compareVersions,
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
};
