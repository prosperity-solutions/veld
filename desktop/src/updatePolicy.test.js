const test = require("node:test");
const assert = require("node:assert/strict");

const {
  compareVersions,
  downloadOnlyReason,
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
  // A prerelease suffix parses as its numeric prefix, matching the CLI's
  // `is_newer` — deliberately, since neither side ever publishes one.
  assert.equal(compareVersions("12.4.0-rc.1", "12.4.0"), 0);
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
