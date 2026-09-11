const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const {
  CONSOLE_HANDOFF,
  DEFAULT_UPDATE_FREQUENCY,
  FULL_UPDATE_HANDOFF,
  REPORT_MAX_AGE_MS,
  UPDATE_PHASE_TIMEOUT_MS,
  DECLINE_EXPIRY_MS,
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
} = require("./updatePolicy");

// A `phase_at` the staleness rule will accept, unless a test wants otherwise.
const liveState = (over = {}) => ({
  pid: 4242,
  origin: "console",
  version: "16.12.0",
  phase: "installing",
  phase_at: new Date().toISOString(),
  ...over,
});

test("an unpackaged run never updates itself", () => {
  for (const platform of ["darwin", "linux"]) {
    assert.equal(
      updateMode({ platform, isPackaged: false, env: { APPIMAGE: "/x.AppImage" } }),
      "off",
    );
  }
});

test("macOS is download-only until the app is signed", () => {
  assert.equal(updateMode({ platform: "darwin", isPackaged: true }), "download");
  // The other side of the switch, so flipping MACOS_SIGNED is a change this
  // suite has already checked rather than one it silently accepts.
  assert.equal(
    updateMode({ platform: "darwin", isPackaged: true, macSigned: true }),
    "install",
  );
  // Signing says nothing about Linux packaging.
  assert.equal(
    updateMode({ platform: "linux", isPackaged: true, env: {}, macSigned: true }),
    "download",
  );
});

test("the veld CLI takes the macOS update when there is one to take it", () => {
  const cli = "/Users/x/.local/bin/veld";
  assert.equal(updateMode({ platform: "darwin", isPackaged: true, cli }), "cli");
  // Outranks Squirrel even once the app is signed: the CLI is what keeps the app
  // and the CLI on one version, which is what the release promises.
  assert.equal(
    updateMode({ platform: "darwin", isPackaged: true, cli, macSigned: true }),
    "cli",
  );
  // No CLI on the machine → unchanged behaviour, both sides of the signing switch.
  assert.equal(updateMode({ platform: "darwin", isPackaged: true }), "download");
  assert.equal(
    updateMode({ platform: "darwin", isPackaged: true, cli: null, macSigned: true }),
    "install",
  );
  // An unpackaged run has no bundle to replace, CLI or not.
  assert.equal(updateMode({ platform: "darwin", isPackaged: false, cli }), "off");
  // macOS only: the Linux AppImage already replaces itself, and a .deb belongs to
  // the package manager whatever else is installed.
  assert.equal(
    updateMode({ platform: "linux", isPackaged: true, env: {}, cli }),
    "download",
  );
  assert.equal(
    updateMode({
      platform: "linux",
      isPackaged: true,
      env: { APPIMAGE: "/opt/Veld.AppImage" },
      cli,
    }),
    "install",
  );
});

test("only an AppImage can install in place on Linux", () => {
  assert.equal(
    updateMode({
      platform: "linux",
      isPackaged: true,
      env: { APPIMAGE: "/opt/Veld.AppImage" },
    }),
    "install",
  );
  // A .deb install: the files belong to dpkg, and there is no APPIMAGE.
  assert.equal(updateMode({ platform: "linux", isPackaged: true, env: {} }), "download");
  assert.equal(updateMode({ platform: "linux", isPackaged: true }), "download");
});

test("download-only says why, per platform", () => {
  // The two platforms are download-only for unrelated reasons; one string
  // covering both is wrong on whichever it wasn't written for.
  assert.match(downloadOnlyReason({ platform: "darwin" }), /code-signed/);
  assert.match(downloadOnlyReason({ platform: "linux" }), /package manager/);
  assert.doesNotMatch(downloadOnlyReason({ platform: "linux" }), /code-signed/);
  assert.ok(downloadOnlyReason({ platform: "freebsd" }).length > 0);
});

test("compareVersions orders major, minor and patch", () => {
  assert.ok(compareVersions("12.4.0", "12.3.9") > 0);
  assert.ok(compareVersions("2.0.0", "12.0.0") < 0);
  assert.equal(compareVersions("12.4.0", "12.4.0"), 0);
  assert.ok(compareVersions("12.4.1", "12.4.0") > 0);
});

test("compareVersions tolerates a v prefix, short and junk versions", () => {
  assert.equal(compareVersions("v12.4.0", "12.4.0"), 0);
  assert.equal(compareVersions("12.4", "12.4.0"), 0);
  assert.equal(compareVersions("", "0.0.0"), 0);
  // A prerelease suffix parses as its numeric prefix — which is where this
  // stops matching the CLI's `is_newer` (Rust rejects the whole component and
  // reads 0). Pinned rather than asserted-as-parity, since neither side ever
  // publishes one; see the note on `compareVersions`.
  assert.equal(compareVersions("12.4.0-rc.1", "12.4.0"), 0);
  assert.equal(compareVersions("12.4.5-rc.1", "12.4.5"), 0);
});

test("versionSkew names the half that is behind", () => {
  assert.deepEqual(
    versionSkew({ appVersion: "12.5.0", daemonVersion: "12.4.0", isPackaged: true }),
    { behind: "daemon", appVersion: "12.5.0", daemonVersion: "12.4.0" },
  );
  assert.deepEqual(
    versionSkew({ appVersion: "12.4.0", daemonVersion: "12.5.0", isPackaged: true }),
    { behind: "app", appVersion: "12.4.0", daemonVersion: "12.5.0" },
  );
});

test("versionSkew stays quiet when it cannot mean anything", () => {
  // Matching versions.
  assert.equal(
    versionSkew({ appVersion: "12.4.0", daemonVersion: "12.4.0", isPackaged: true }),
    null,
  );
  // A dev build is 0.0.0 and would otherwise report skew against every daemon.
  assert.equal(
    versionSkew({ appVersion: "0.0.0", daemonVersion: "12.4.0", isPackaged: false }),
    null,
  );
  // A daemon too old to report a version.
  assert.equal(
    versionSkew({ appVersion: "12.4.0", daemonVersion: undefined, isPackaged: true }),
    null,
  );
  // Anything that isn't a version string. Whatever answers /api/health is not
  // necessarily veld's daemon, and a non-string would otherwise become a Set key
  // that never matches the next poll's — re-notifying every minute forever.
  for (const junk of [{}, [], 12, true, null]) {
    assert.equal(
      versionSkew({ appVersion: "12.4.0", daemonVersion: junk, isPackaged: true }),
      null,
      `daemonVersion ${JSON.stringify(junk)} must not report skew`,
    );
  }
});

test("releasePageUrl points at the tag, or at latest without one", () => {
  assert.equal(
    releasePageUrl("12.5.0"),
    "https://github.com/prosperity-solutions/veld/releases/tag/v12.5.0",
  );
  assert.equal(
    releasePageUrl("v12.5.0"),
    "https://github.com/prosperity-solutions/veld/releases/tag/v12.5.0",
  );
  assert.equal(
    releasePageUrl(undefined),
    "https://github.com/prosperity-solutions/veld/releases/latest",
  );
});

test("CLI candidates are the installer's own directories, system prefix first", () => {
  // Scoped deliberately. An earlier version of this test asserted the order was
  // a *trust* boundary — "the user-writable candidate must be last" — which is
  // false: /opt/homebrew/bin is drwxrwxr-x <user>:admin on Apple Silicon and
  // ranks above ~/.local/bin, and anything able to write to any of these can
  // replace the real veld anyway. What the order genuinely pins is agreement
  // with install.sh, so that the app resolves the binary the installer wrote.
  assert.deepEqual(cliCandidatePaths({ home: "/Users/x" }), [
    "/usr/local/bin/veld",
    "/opt/homebrew/bin/veld",
    "/Users/x/.local/bin/veld",
  ]);
});

test("looksLikeVeldCli accepts the CLI's own --version and nothing looser", () => {
  // What `veld --version` actually prints (clap).
  assert.equal(looksLikeVeldCli("veld 16.6.0"), true);
  assert.equal(looksLikeVeldCli("  veld 16.6.0\n"), true);
  assert.equal(looksLikeVeldCli("veld v16.6.0"), true);

  // The point of the check: being executable and being named `veld` is not the
  // same as being veld, and this is the only thing standing between the two.
  assert.equal(looksLikeVeldCli("veld"), false);
  assert.equal(looksLikeVeldCli("veldctl 1.2.3"), false);
  assert.equal(looksLikeVeldCli("this is not veld 1.2.3"), false);
  assert.equal(looksLikeVeldCli("bash: veld: command not found"), false);
  assert.equal(looksLikeVeldCli(""), false);
  for (const junk of [null, undefined, {}, 12, ["veld 1.0.0"]]) {
    assert.equal(looksLikeVeldCli(junk), false, `${JSON.stringify(junk)} is not veld`);
  }
});

test("a handoff report expires, so yesterday's failure is not announced today", () => {
  const now = Date.parse("2026-08-07T05:00:00Z");

  // The report the app just came back from.
  assert.equal(
    reportIsFresh({ finishedAt: "2026-08-07T04:59:55Z", now }),
    true,
    "a report written five seconds ago is this launch's",
  );

  // The bug this exists for, with the real values off the machine it happened
  // on: a failed 99.0.0 handoff left a report in ~/.veld, and the next launch —
  // a different install, a day later, running 16.7.0 — announced it.
  assert.equal(
    reportIsFresh({ finishedAt: "2026-08-06T14:45:59.044305+00:00", now }),
    false,
    "a report from the previous day must never be announced",
  );

  // Boundaries.
  assert.equal(reportIsFresh({ finishedAt: "2026-08-07T04:46:00Z", now }), true);
  assert.equal(reportIsFresh({ finishedAt: "2026-08-07T04:44:00Z", now }), false);

  // Clock skew is tolerated the same amount in both directions: a stamp slightly
  // in the future is a machine whose clock moved, not a lie.
  assert.equal(reportIsFresh({ finishedAt: "2026-08-07T05:05:00Z", now }), true);
  assert.equal(reportIsFresh({ finishedAt: "2026-09-01T00:00:00Z", now }), false);

  // A report that cannot say when it was written cannot claim to be about this
  // launch. Staying quiet is the cheaper mistake.
  for (const junk of [undefined, null, "", "not a date", 12, {}, []]) {
    assert.equal(
      reportIsFresh({ finishedAt: junk, now }),
      false,
      `finished_at ${JSON.stringify(junk)} must not count as fresh`,
    );
  }

  assert.equal(typeof REPORT_MAX_AGE_MS, "number");
  assert.ok(REPORT_MAX_AGE_MS > 0);
});

test("a CLI that advertises nothing gets the app-only command", () => {
  const ctx = { version: "16.8.0", pid: 4321, execPath: "/Applications/Veld.app/Contents/MacOS/Veld" };

  // No capabilities key, unparseable output, a non-array, non-string members —
  // every one of these must land on the command an older CLI understands. The
  // failure being avoided is spawning `veld update --wait-pid` at a CLI that
  // rejects the flag, exits 2, and leaves the user with no window at all.
  for (const stdout of [
    undefined,
    null,
    "",
    "not json",
    "{}",
    '{"capabilities": "full-update-handoff"}',
    '{"capabilities": null}',
    '{"capabilities": [42, {"a": 1}]}',
  ]) {
    const capabilities = capabilitiesFrom(stdout);
    assert.deepEqual(capabilities, [], `capabilities from ${JSON.stringify(stdout)}`);
    const { args, full } = handoffCommand({ capabilities, ...ctx });
    assert.equal(full, false);
    assert.deepEqual(args, [
      "desktop",
      "update",
      // Required on this path and only this path: without it an older CLI
      // reinstalls its own version, relaunches, is offered the newer one again,
      // and never converges.
      "--version",
      "16.8.0",
      "--wait-pid",
      "4321",
      "--relaunch",
      "--app-path",
      "/Applications/Veld.app/Contents/MacOS/Veld",
    ]);
  }
});

test("a CLI that advertises the handoff updates the whole release", () => {
  const stdout = JSON.stringify({
    installed: true,
    version: "16.7.1",
    capabilities: [FULL_UPDATE_HANDOFF, CONSOLE_HANDOFF, "some-future-thing"],
  });
  const capabilities = capabilitiesFrom(stdout);
  assert.deepEqual(capabilities, [
    FULL_UPDATE_HANDOFF,
    CONSOLE_HANDOFF,
    "some-future-thing",
  ]);

  const { args, full } = handoffCommand({
    capabilities,
    version: "16.8.0",
    pid: 99,
    execPath: "/Users/x/Applications/Veld.app/Contents/MacOS/Veld",
  });
  assert.equal(full, true);
  assert.deepEqual(args, [
    "update",
    // The reason this route is worth having at all now: the CLI re-runs itself
    // in a terminal window, so the user sees the 1–4 minutes after this app
    // quits, and `sudo` has somewhere to ask for the password a privileged
    // install needs.
    "--console",
    // The release the app was offered, from the feed that offered it. Without
    // it the CLI asks api.github.com — a second source, rate-limited per IP and
    // briefly out of step with the feed after a release.
    "--target-version",
    "16.8.0",
    "--wait-pid",
    "99",
    "--relaunch",
    "--app-path",
    "/Users/x/Applications/Veld.app/Contents/MacOS/Veld",
  ]);
  // Spelled differently from the app-only route's `--version` on purpose: the
  // two mean different things ("install this release" vs "install this app
  // build"), and a CLI old enough to know only the latter must reject this
  // outright rather than half-understand it.
  assert.equal(args.includes("--version"), false);
});

test("the button never promises more than the mode can deliver", () => {
  // Only the full handoff may name the CLI: every other route moves the app
  // alone, and a label that says "veld" would be a claim nothing behind it honours.
  assert.equal(
    primaryAction({ viaCli: true, canInstall: false, full: true }),
    "Quit and Update veld",
  );
  assert.equal(
    primaryAction({ viaCli: true, canInstall: false, full: false }),
    "Quit and Update",
  );
  assert.equal(
    primaryAction({ viaCli: false, canInstall: true, full: false }),
    "Download and Install",
  );
  assert.equal(
    primaryAction({ viaCli: false, canInstall: false, full: false }),
    "Open Release Page",
  );
  // `full` is meaningless without the CLI route, and must not leak into it.
  assert.equal(
    primaryAction({ viaCli: false, canInstall: true, full: true }),
    "Download and Install",
  );
});

test("a live update stops the app from opening over it", () => {
  const running = updateInProgress({ state: liveState(), pidAlive: () => true });
  assert.deepEqual(running, {
    pid: 4242,
    phase: "installing",
    version: "16.12.0",
    origin: "console",
  });
});

test("both staleness conditions free the app, and neither alone is enough", () => {
  const now = Date.now();
  // Dead holder, timestamp fresh — only the liveness check can catch this.
  assert.equal(
    updateInProgress({ state: liveState(), now, pidAlive: () => false }),
    null,
  );
  // Live holder, timestamp old — the wedged-at-a-sudo-prompt case, which a
  // liveness check cannot see at all.
  assert.equal(
    updateInProgress({
      state: liveState({ phase_at: new Date(now - UPDATE_PHASE_TIMEOUT_MS - 1000).toISOString() }),
      now,
      pidAlive: () => true,
    }),
    null,
  );
  // Neither: still running.
  assert.notEqual(
    updateInProgress({
      state: liveState({ phase_at: new Date(now - UPDATE_PHASE_TIMEOUT_MS + 60_000).toISOString() }),
      now,
      pidAlive: () => true,
    }),
    null,
  );
});

test("a clock that moved forwards is not read as abandonment", () => {
  const now = Date.now();
  assert.notEqual(
    updateInProgress({
      state: liveState({ phase_at: new Date(now + 2 * 60 * 60 * 1000).toISOString() }),
      now,
      pidAlive: () => true,
    }),
    null,
  );
});

test("garbage in ~/.veld never makes the app unopenable", () => {
  // Every one of these must read as "no update": the consequence of this answer
  // is that Veld Desktop opens, and a malformed file must not be able to lock a
  // user out of their app.
  for (const state of [
    null,
    undefined,
    "nope",
    42,
    {},
    { pid: "4242", phase_at: new Date().toISOString() },
    { pid: 0, phase_at: new Date().toISOString() },
    { pid: -1, phase_at: new Date().toISOString() },
    { pid: 4242 },
    { pid: 4242, phase_at: "not a date" },
  ]) {
    assert.equal(updateInProgress({ state, pidAlive: () => true }), null, JSON.stringify(state));
  }
});

test("an unknown phase from a newer CLI still says something true", () => {
  // The app and the CLI ship together, but a user can end up with a newer CLI
  // and an older app for exactly the minutes this dialog exists to cover.
  assert.equal(updatePhaseLabel("something-invented-later"), "in progress");
  assert.equal(updatePhaseLabel("restarting-services"), "restarting the daemon and helper");
});

test("every phase the Rust side can write has its own wording here", () => {
  // **Read out of `update_lock.rs` rather than restated.** A hand-copied list
  // makes this test a claim it cannot hold: the first version of it enumerated
  // six phases, silently omitted `starting` — which `acquire` writes on every
  // single update — and passed. A seventh variant added in Rust would have shipped
  // green too. This is the drift gate, in the shape `install_script_contract.rs`
  // uses for the same reason.
  const rust = fs.readFileSync(
    path.join(__dirname, "..", "..", "crates", "veld-core", "src", "update_lock.rs"),
    "utf8",
  );
  const asStr = rust.slice(
    rust.indexOf("impl Phase {"),
    rust.indexOf("/// One human clause"),
  );
  const phases = [...asStr.matchAll(/Phase::\w+ => "([a-z-]+)"/g)].map((m) => m[1]);

  assert.ok(phases.length >= 7, `found only ${phases.length} phases — did the parse break?`);
  assert.ok(phases.includes("starting"), "the phase `acquire` writes must be in the list");
  // `unknown` is Rust's `#[serde(other)]` catch-all — the variant an OLD binary
  // produces when it reads a NEW one's state file. Falling through to the
  // default wording is the correct answer for it and the only honest one, so it
  // is exempted by name rather than by loosening the rule for everything else.
  assert.ok(phases.includes("unknown"), "the serde catch-all must still exist");
  for (const phase of phases.filter((p) => p !== "unknown")) {
    assert.notEqual(
      updatePhaseLabel(phase),
      "in progress",
      `Phase::…"${phase}" falls through to the default label`,
    );
  }
});

test("an old CLI is never handed --console", () => {
  // The skew is real, not theoretical: `veld desktop update` moves the app half
  // *alone*, so a new app can be driving an old CLI. That CLI advertises
  // full-update-handoff — it has always had those flags — but its clap rejects
  // `--console` with a usage error and exit 2, after this app has already quit
  // and with no report written. The user would reopen on the old version having
  // been told nothing.
  const { args, full } = handoffCommand({
    capabilities: [FULL_UPDATE_HANDOFF],
    version: "16.12.0",
    pid: 99,
    execPath: "/Applications/Veld.app/Contents/MacOS/Veld",
  });
  assert.equal(full, true, "the full route is still taken");
  assert.equal(args.includes("--console"), false);
  assert.deepEqual(args.slice(0, 2), ["update", "--target-version"]);
});

test("a CLI that advertises console-handoff gets the terminal window", () => {
  const { args } = handoffCommand({
    capabilities: [FULL_UPDATE_HANDOFF, CONSOLE_HANDOFF],
    version: "16.12.0",
    pid: 99,
    execPath: "/Applications/Veld.app/Contents/MacOS/Veld",
  });
  assert.deepEqual(args.slice(0, 2), ["update", "--console"]);
});

test("console-handoff alone never invents the full route", () => {
  // The two capabilities are independent, and `--console` is only ever a
  // modifier on `veld update`. A CLI too old for the full handoff must still get
  // `veld desktop update`, with no stray flag on it.
  const { args, full } = handoffCommand({
    capabilities: [CONSOLE_HANDOFF],
    version: "16.12.0",
    pid: 99,
    execPath: "/Applications/Veld.app/Contents/MacOS/Veld",
  });
  assert.equal(full, false);
  assert.equal(args.includes("--console"), false);
  assert.deepEqual(args.slice(0, 2), ["desktop", "update"]);
});

// ---------------------------------------------------------------------------
// Update frequency tiers
// ---------------------------------------------------------------------------

const HOUR = 60 * 60 * 1000;
const NOW = Date.parse("2026-09-11T12:00:00Z");

test("the default tier is quieter than the behaviour it replaces", () => {
  // The whole point of the change, asserted rather than described: same check
  // interval, but a release has to ripen and a day has to pass between prompts.
  // A regression that made the default prompt eagerly would otherwise be
  // invisible — every individual dialog still looks correct.
  const before = UPDATE_TIERS.balanced;
  assert.equal(DEFAULT_UPDATE_FREQUENCY, "balanced");
  assert.equal(before.checkIntervalMs, 6 * HOUR);
  assert.ok(before.minReleaseAgeMs > 0);
  assert.ok(before.minPromptGapMs >= 24 * HOUR);
  assert.equal(UPDATE_TIERS.eager.minReleaseAgeMs, 0);
  assert.equal(UPDATE_TIERS.eager.minPromptGapMs, 0);
  // Each tier is strictly quieter than the one above it on every axis a person
  // can perceive, so the labels stay true in both directions.
  for (const axis of ["minReleaseAgeMs", "minPromptGapMs", "versionsAheadOverride"]) {
    assert.ok(UPDATE_TIERS.eager[axis] <= UPDATE_TIERS.balanced[axis], axis);
    assert.ok(UPDATE_TIERS.balanced[axis] <= UPDATE_TIERS.relaxed[axis], axis);
  }
});

test("an unknown tier name is the default, in both directions", () => {
  // A newer daemon can offer a tier this app has never heard of, and a garbled
  // value must not leave the updater with no schedule at all.
  assert.equal(updateTier("relaxed"), UPDATE_TIERS.relaxed);
  assert.equal(updateTier("glacial"), UPDATE_TIERS.balanced);
  assert.equal(updateTier(undefined), UPDATE_TIERS.balanced);
  assert.equal(updateTier(7), UPDATE_TIERS.balanced);
  // Not `Object.prototype`'s, either: a stored "constructor" must not resolve.
  assert.equal(updateTier("constructor"), UPDATE_TIERS.balanced);
});

test("updateFrequencyFrom only moves off the fallback for a tier it knows", () => {
  assert.equal(updateFrequencyFrom({ settings: { "desktop.updateFrequency": "eager" } }), "eager");
  assert.equal(updateFrequencyFrom({ settings: {} }, "relaxed"), "relaxed");
  assert.equal(updateFrequencyFrom(null, "relaxed"), "relaxed");
  assert.equal(updateFrequencyFrom({ settings: { "desktop.updateFrequency": 3 } }), "balanced");
  // An older app against a newer daemon: keep what we had rather than guessing.
  assert.equal(
    updateFrequencyFrom({ settings: { "desktop.updateFrequency": "nightly" } }, "relaxed"),
    "relaxed",
  );
});

test("a release's age survives a laptop that was shut", () => {
  // `firstSeenAt` alone would restart the ripening clock every time the app
  // reopens, so a release that has been out for a week would be treated as new.
  const age = releaseAgeMs({
    releaseDate: "2026-09-04T12:00:00Z",
    firstSeenAt: NOW,
    now: NOW,
  });
  assert.equal(age, 7 * 24 * HOUR);
});

test("a release with no usable date falls back to when it was first seen", () => {
  const seen = NOW - 5 * HOUR;
  assert.equal(releaseAgeMs({ releaseDate: undefined, firstSeenAt: seen, now: NOW }), 5 * HOUR);
  assert.equal(releaseAgeMs({ releaseDate: "not a date", firstSeenAt: seen, now: NOW }), 5 * HOUR);
  // A publisher's clock ahead of ours is not a release from tomorrow, and must
  // never produce a negative age that reads as "ripe".
  assert.equal(
    releaseAgeMs({ releaseDate: "2026-09-12T12:00:00Z", firstSeenAt: seen, now: NOW }),
    5 * HOUR,
  );
  // Nothing known at all is age zero, i.e. "not ripe" — the quiet direction.
  assert.equal(releaseAgeMs({ now: NOW }), 0);
});

test("versionsAhead counts only what the running version has not caught up with", () => {
  const seen = { "16.70.0": 1, "16.71.0": 2, "16.72.0": 3, "16.73.0": 4 };
  assert.equal(versionsAhead({ seen, currentVersion: "16.71.0" }), 2);
  assert.equal(versionsAhead({ seen, currentVersion: "16.73.0" }), 0);
  assert.equal(versionsAhead({ seen: null, currentVersion: "16.71.0" }), 0);
});

test("the default tier waits for a release to settle, then offers it", () => {
  const tier = UPDATE_TIERS.balanced;
  const base = { tier, ahead: 1, lastPromptedAt: null, now: NOW };
  assert.deepEqual(shouldOfferUpdate({ ...base, ageMs: 6 * HOUR }), {
    offer: false,
    reason: "ripening",
  });
  assert.deepEqual(shouldOfferUpdate({ ...base, ageMs: 40 * HOUR }), {
    offer: true,
    reason: "aged",
  });
});

test("releases piling up shortcut the age gate but never the prompt gap", () => {
  // The distinction the tiers turn on: a burst is a reason to stop waiting for
  // the current release to settle, not a reason to prompt twice in an hour.
  const tier = UPDATE_TIERS.balanced;
  assert.deepEqual(shouldOfferUpdate({ tier, ageMs: 1 * HOUR, ahead: 4, now: NOW }), {
    offer: true,
    reason: "piled-up",
  });
  assert.deepEqual(
    shouldOfferUpdate({
      tier,
      ageMs: 1 * HOUR,
      ahead: 40,
      lastPromptedAt: NOW - 2 * HOUR,
      now: NOW,
    }),
    { offer: false, reason: "too-soon" },
  );
});

test("a manual check is answered whatever the tier says", () => {
  // Clicking Check for Updates… is the one input that outranks every gate,
  // including a decline — asking again is the whole point of clicking it.
  const tier = UPDATE_TIERS.relaxed;
  assert.deepEqual(
    shouldOfferUpdate({
      tier,
      ageMs: 0,
      ahead: 0,
      declinedAt: NOW,
      lastPromptedAt: NOW - 60_000,
      manual: true,
      now: NOW,
    }),
    { offer: true, reason: "manual" },
  );
});

test("a declined release stays declined for the automatic check", () => {
  assert.deepEqual(
    shouldOfferUpdate({
      tier: UPDATE_TIERS.eager,
      ageMs: 99 * HOUR,
      ahead: 9,
      declinedAt: NOW - HOUR,
      now: NOW,
    }),
    { offer: false, reason: "declined" },
  );
});

test("\"Later\" means later, not never", () => {
  // The feed only ever names the newest release, so a decline that never expired
  // would mean an automatic check never raises that version again — on a quiet
  // week, never at all, from a button labelled Later.
  const base = { tier: UPDATE_TIERS.balanced, ageMs: 99 * HOUR, ahead: 1, now: NOW };
  assert.equal(shouldOfferUpdate({ ...base, declinedAt: NOW - DECLINE_EXPIRY_MS + HOUR }).offer, false);
  assert.equal(shouldOfferUpdate({ ...base, declinedAt: NOW - DECLINE_EXPIRY_MS - HOUR }).offer, true);
});

test("a decline timestamp from the future does not silence the app forever", () => {
  // Same clock tolerance as `reportIsFresh`, and here it matters more: a
  // `declinedAt` years ahead would mute that version permanently, which is the
  // exact failure the expiry was added to prevent.
  assert.equal(declineHolds({ declinedAt: NOW + 30 * 24 * HOUR, now: NOW }), false);
  assert.equal(declineHolds({ declinedAt: NOW + HOUR, now: NOW }), true);
  assert.equal(declineHolds({ declinedAt: null, now: NOW }), false);
  assert.equal(declineHolds({ declinedAt: "yesterday", now: NOW }), false);
});

test("a clock that jumped backwards does not silence the app for days", () => {
  // One-sided, like `updateInProgress`'s phase check. A `lastPromptedAt` in the
  // future read as "the gap has not elapsed" would mute updates until the
  // timestamp caught up.
  assert.deepEqual(
    shouldOfferUpdate({
      tier: UPDATE_TIERS.balanced,
      ageMs: 99 * HOUR,
      ahead: 1,
      lastPromptedAt: NOW + 30 * 24 * HOUR,
      now: NOW,
    }),
    { offer: true, reason: "aged" },
  );
});

test("the eager tier offers a release the moment it is seen", () => {
  assert.deepEqual(
    shouldOfferUpdate({ tier: UPDATE_TIERS.eager, ageMs: 0, ahead: 1, now: NOW }),
    { offer: true, reason: "piled-up" },
  );
});

test("nudge state is pruned to the running version", () => {
  // Without this the `seen` map is append-only for the life of an install, and —
  // worse — the four releases that triggered a prompt would still be counted
  // against the release they installed.
  const state = pruneUpdateState(
    {
      lastPromptedAt: 1234,
      seen: { "16.70.0": 1, "16.72.0": 2, "16.73.0": 3 },
      declined: { "16.70.0": 9, "16.73.0": 10 },
    },
    "16.72.0",
  );
  assert.deepEqual(state, {
    lastPromptedAt: 1234,
    seen: { "16.73.0": 3 },
    declined: { "16.73.0": 10 },
  });
});

test("a malformed nudge file degrades to nothing known", () => {
  // It lives in userData where anything can edit it, and an updater that throws
  // on every check is a worse outcome than one extra prompt.
  const empty = { lastPromptedAt: null, seen: {}, declined: {} };
  assert.deepEqual(pruneUpdateState(null, "16.72.0"), empty);
  assert.deepEqual(pruneUpdateState("nonsense", "16.72.0"), empty);
  assert.deepEqual(pruneUpdateState({ seen: [], declined: "16.73.0" }, "16.72.0"), empty);
  // An array is the shape this field had while the feature was being written,
  // and it carries no timestamps — so it cannot answer the expiry question and
  // is dropped rather than half-honoured.
  assert.deepEqual(pruneUpdateState({ declined: ["16.73.0"] }, "16.72.0"), empty);
  assert.deepEqual(
    pruneUpdateState({ lastPromptedAt: "soon", seen: { "16.73.0": "yes" } }, "16.72.0"),
    empty,
  );
});
