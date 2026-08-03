// electron-builder `afterPack` hook: ad-hoc sign the macOS bundle when nothing
// else will sign it.
//
// Not a substitute for Developer ID signing, which release CI now does (issue
// #167 §10) — an ad-hoc signature carries no identity, so Gatekeeper still
// quarantines the download and the user still has to open it explicitly the first
// time. What it fixes is worse than a warning: on Apple Silicon every executable
// must carry *some* valid signature to run at all, and repacking Electron (asar,
// rename, icon) invalidates the one the prebuilt binaries came with. An unsigned
// bundle there does not warn — it reports "Veld is damaged and can't be opened",
// which reads as a corrupt download and has no in-UI way out.
//
// #167 §10 planned to delete this hook once a Developer ID existed. It cannot go,
// because electron-builder does not sign on the two paths that need it most:
//
//   - **Every PR build.** `isSignAllowed()` bails as soon as `GITHUB_BASE_REF` is
//     set (builder-util's `isPullRequest`), before it ever looks for an identity.
//     Whatever `mac.identity` says, a PR artifact can only be signed here.
//   - **Contributor machines**, which have no Developer ID Application
//     certificate; electron-builder logs a warning and produces an unsigned app.
//
// So the hook stays and steps aside instead. Signing twice is not additive:
// electron-builder's pass would replace the outer signature while any nested
// binary its own walk skips would keep an ad-hoc one, and notarization rejects
// exactly that mixture.

const { execFileSync } = require("node:child_process");
const path = require("node:path");

/**
 * Whether electron-builder is about to apply a real signature to this bundle —
 * the one case where this hook must keep its hands off.
 *
 * It answers two of app-builder-lib's own questions (is this a pull-request
 * build; is a Developer ID reachable) and it is **deliberately biased**: where the
 * two could disagree, this says no and signs ad-hoc. The asymmetry is the whole
 * design. A redundant ad-hoc pass is replaced moments later by the real signature
 * and costs nothing; a wrongly-skipped one ships a bundle nobody signed, which
 * reports "Veld is damaged and can't be opened".
 *
 * The keychain probe is not hypothetical: the first machine to hold this project's
 * Developer ID certificate is a maintainer's laptop, and without it every local
 * `just desktop-package` there would double-sign — something CI can never
 * reproduce, since no runner has the certificate outside the release job.
 *
 * `CSC_LINK` is the one unhedged assumption, and it is here because the certificate
 * CI imports lands in a throwaway keychain the probe cannot see. A `.p12` holding no
 * Developer ID Application certificate would make this return true while
 * electron-builder finds nothing and warns — the unsigned outcome above. The release
 * job's own assertions catch that; a maintainer rehearsing the release path locally
 * with an exported `.p12` would not be caught, and would get the damaged-bundle
 * dialog.
 *
 * `CSC_NAME` is deliberately *not* consulted. It is inert while `mac.identity` is
 * set — `findIdentity` prefers the config's qualifier — so reading it would answer a
 * question electron-builder is not asking. The tests keep the behaviour pinned in
 * case that key is ever removed from `electron-builder.yml`.
 *
 * @param {{env?: Record<string, string | undefined>, listIdentities?: () => string}} [deps]
 *   injected by the tests; the defaults are the real environment and keychain.
 */
function packagerWillSignForReal({ env = process.env, listIdentities = findIdentities } = {}) {
  const forcedOnPullRequest = /^(true|1)$/i.test(env.CSC_FOR_PULL_REQUEST ?? "");
  if (env.GITHUB_BASE_REF && !forcedOnPullRequest) return false;
  if (env.CSC_LINK) return true;

  try {
    // `-p codesigning` is the narrower of the two lists app-builder-lib consults,
    // which is the biased direction: a certificate that shows up only in the
    // unfiltered list is one this returns false for, and a redundant ad-hoc
    // signature is the cost of being wrong that way.
    return listIdentities().includes("Developer ID Application:");
  } catch {
    // No keychain to ask, or no `security` binary. Same bias.
    return false;
  }
}

/** The real keychain probe. Separated so `packagerWillSignForReal` is testable. */
function findIdentities() {
  return execFileSync("/usr/bin/security", ["find-identity", "-v", "-p", "codesigning"], {
    encoding: "utf8",
  });
}

exports.default = async function adhocSign(context) {
  if (context.electronPlatformName !== "darwin") return;

  const appPath = path.join(
    context.appOutDir,
    `${context.packager.appInfo.productFilename}.app`,
  );

  if (packagerWillSignForReal()) {
    console.log(
      `  • skipping ad-hoc signature for ${path.basename(appPath)} — a Developer ID signature follows`,
    );
    return;
  }

  // `--deep` is deprecated for real signing (Apple wants each nested binary
  // signed on its own terms) but is exactly right for an ad-hoc pass: the
  // helper apps and frameworks all need the same nothing-identity, and the
  // alternative is re-deriving Electron's nested layout here. `--force`
  // replaces the signature the prebuilt binaries shipped with, which repacking
  // already invalidated.
  execFileSync("codesign", ["--force", "--deep", "--sign", "-", appPath], {
    stdio: "inherit",
  });

  console.log(`  • ad-hoc signed ${path.basename(appPath)}`);
};

// Exported for `adhoc-sign.test.js`. electron-builder only ever calls `default`.
exports.packagerWillSignForReal = packagerWillSignForReal;
