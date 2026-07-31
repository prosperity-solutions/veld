// The branch that decides whether a macOS bundle gets an ad-hoc signature.
//
// Worth testing rather than eyeballing because both wrong answers ship a broken
// artifact and neither is reproducible on a runner: wrong-true ships a bundle
// nobody signed ("Veld is damaged and can't be opened"), wrong-false double-signs
// and can leave nested binaries in a mixture notarization rejects. The keychain
// probe is injected, so this runs anywhere — including the Linux CI leg.

const test = require("node:test");
const assert = require("node:assert/strict");

const { packagerWillSignForReal } = require("./adhoc-sign");

const NO_IDENTITIES = () => "     0 valid identities found\n";
const DEVELOPER_ID = () =>
  '  1) ABC123 "Developer ID Application: Prosperity Solutions (TEAM123456)"\n     1 identities found\n';

test("release CI's imported certificate counts, even though the keychain probe cannot see it", () => {
  assert.equal(
    packagerWillSignForReal({ env: { CSC_LINK: "base64…" }, listIdentities: NO_IDENTITIES }),
    true,
  );
});

test("a Developer ID in the keychain counts", () => {
  assert.equal(packagerWillSignForReal({ env: {}, listIdentities: DEVELOPER_ID }), true);
  assert.equal(packagerWillSignForReal({ env: {}, listIdentities: NO_IDENTITIES }), false);
});

test("a pull-request build is never signed by the packager, whatever is available", () => {
  // `isSignAllowed()` bails on GITHUB_BASE_REF before it looks for an identity, so
  // the ad-hoc pass is the only signature a PR artifact can get.
  assert.equal(
    packagerWillSignForReal({
      env: { GITHUB_BASE_REF: "main", CSC_LINK: "base64…" },
      listIdentities: DEVELOPER_ID,
    }),
    false,
  );
  // …unless the escape hatch app-builder-lib itself honours is set.
  assert.equal(
    packagerWillSignForReal({
      env: { GITHUB_BASE_REF: "main", CSC_FOR_PULL_REQUEST: "true", CSC_LINK: "base64…" },
      listIdentities: NO_IDENTITIES,
    }),
    true,
  );
});

// `CSC_NAME` is inert as long as `electron-builder.yml` sets `mac.identity`, since
// `findIdentity` prefers the config's qualifier. This pins the behaviour for the day
// that key is removed, when a mistyped CSC_NAME would otherwise make the hook stand
// down while electron-builder's own lookup finds nothing.
test("CSC_NAME is not evidence — a typo in it must not skip the ad-hoc pass", () => {
  assert.equal(
    packagerWillSignForReal({
      env: { CSC_NAME: "Developer ID Application: Typo" },
      listIdentities: NO_IDENTITIES,
    }),
    false,
  );
});

test("an unusable keychain means ad-hoc, not a guess", () => {
  assert.equal(
    packagerWillSignForReal({
      env: {},
      listIdentities: () => {
        throw new Error("SecKeychainCopySearchList: some error");
      },
    }),
    false,
  );
});
