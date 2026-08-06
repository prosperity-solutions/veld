const test = require("node:test");
const assert = require("node:assert/strict");

const {
  cliCandidatePaths,
  compareVersions,
  downloadOnlyReason,
  looksLikeVeldCli,
  releasePageUrl,
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

test("the user-writable CLI directory is probed last", () => {
  // Not a style preference: whatever `findCli` returns is spawned *detached* by
  // a GUI app. `~/.local/bin` is writable by anything already running as this
  // user, the two system prefixes are not — so a dropped file named `veld` may
  // only win on a machine that has no system install at all.
  const paths = cliCandidatePaths({ home: "/Users/x" });
  assert.deepEqual(paths, [
    "/usr/local/bin/veld",
    "/opt/homebrew/bin/veld",
    "/Users/x/.local/bin/veld",
  ]);
  assert.equal(
    paths.indexOf("/Users/x/.local/bin/veld"),
    paths.length - 1,
    "the user-writable candidate must be last, or the trust order is inverted",
  );
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
