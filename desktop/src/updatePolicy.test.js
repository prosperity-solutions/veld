const test = require("node:test");
const assert = require("node:assert/strict");

const {
  compareVersions,
  downloadOnlyReason,
  isDeveloperIdSigned,
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

test("a signed macOS build installs in place, an unsigned one cannot", () => {
  assert.equal(
    updateMode({ platform: "darwin", isPackaged: true, macSigned: true }),
    "install",
  );
  assert.equal(
    updateMode({ platform: "darwin", isPackaged: true, macSigned: false }),
    "download",
  );
  // Nobody measured → the answer that costs a download button rather than a
  // download the platform will then refuse to apply.
  assert.equal(updateMode({ platform: "darwin", isPackaged: true }), "download");
  // Signing says nothing about Linux packaging.
  assert.equal(
    updateMode({ platform: "linux", isPackaged: true, env: {}, macSigned: true }),
    "download",
  );
});

test("isDeveloperIdSigned reads codesign's report, not a build-time promise", () => {
  // Trimmed `codesign --display --verbose=2` output, in the two shapes that decide
  // whether Squirrel.Mac can swap this bundle.
  const signed = [
    "Identifier=dev.veld.desktop",
    "Signature size=9046",
    "Authority=Developer ID Application: Prosperity Solutions (TEAM123456)",
    "Authority=Developer ID Certification Authority",
    "TeamIdentifier=TEAM123456",
  ].join("\n");
  const adhoc = [
    "Identifier=dev.veld.desktop",
    "CodeDirectory v=20400 size=297 flags=0x2(adhoc) hashes=3+3 location=embedded",
    "Signature=adhoc",
    "TeamIdentifier=not set",
  ].join("\n");

  assert.equal(isDeveloperIdSigned(signed), true);
  assert.equal(isDeveloperIdSigned(adhoc), false);
  // Not a substring match: an Authority further down the chain, or the string
  // appearing inside some other field, must not pass for the leaf certificate.
  assert.equal(
    isDeveloperIdSigned("Authority=Developer ID Certification Authority"),
    false,
  );
  assert.equal(isDeveloperIdSigned("x=Authority=Developer ID Application: nope"), false);
  // A failed probe hands this whatever it got, including nothing at all.
  assert.equal(isDeveloperIdSigned(""), false);
  assert.equal(isDeveloperIdSigned(undefined), false);
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
  assert.match(downloadOnlyReason({ platform: "darwin" }), /Developer ID/);
  assert.match(downloadOnlyReason({ platform: "linux" }), /package manager/);
  assert.doesNotMatch(downloadOnlyReason({ platform: "linux" }), /Developer ID/);
  // It describes *this build*, not veld: releases are signed, so a sentence
  // claiming otherwise would be false on the only path that can reach it.
  assert.doesNotMatch(downloadOnlyReason({ platform: "darwin" }), /yet/);
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
