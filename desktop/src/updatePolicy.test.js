const test = require("node:test");
const assert = require("node:assert/strict");

const {
  FULL_UPDATE_HANDOFF,
  REPORT_MAX_AGE_MS,
  capabilitiesFrom,
  cliCandidatePaths,
  compareVersions,
  downloadOnlyReason,
  handoffCommand,
  looksLikeVeldCli,
  primaryAction,
  releasePageUrl,
  reportIsFresh,
  updateMode,
  versionSkew,
} = require("./updatePolicy");

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
    capabilities: [FULL_UPDATE_HANDOFF, "some-future-thing"],
  });
  const capabilities = capabilitiesFrom(stdout);
  assert.deepEqual(capabilities, [FULL_UPDATE_HANDOFF, "some-future-thing"]);

  const { args, full } = handoffCommand({
    capabilities,
    version: "16.8.0",
    pid: 99,
    execPath: "/Users/x/Applications/Veld.app/Contents/MacOS/Veld",
  });
  assert.equal(full, true);
  assert.deepEqual(args, [
    "update",
    "--wait-pid",
    "99",
    "--relaunch",
    "--app-path",
    "/Users/x/Applications/Veld.app/Contents/MacOS/Veld",
  ]);
  // `veld update` resolves the release itself, so pinning a version here would
  // be a second opinion about which release is current — and the flag does not
  // exist on that subcommand.
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
