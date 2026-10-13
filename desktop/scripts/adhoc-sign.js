// electron-builder `afterPack` hook: ad-hoc sign the macOS bundle — for every
// build that is not about to be signed for real.
//
// Not a substitute for Developer ID signing — an ad-hoc signature carries no
// identity, so Gatekeeper still quarantines the download and the user still has
// to open it explicitly the first time. What it fixes is worse than a warning:
// on Apple Silicon every executable must carry *some* valid signature to run at
// all, and repacking Electron (asar, rename, icon) invalidates the one the
// prebuilt binaries came with. An unsigned bundle there does not warn — it
// reports "Veld is damaged and can't be opened", which reads as a corrupt
// download and has no in-UI way out.
//
// Release CI is the one build that signs for real: it overrides `mac.identity`
// (null in electron-builder.yml) on the command line, and electron-builder then
// signs with the Developer ID and notarizes. This hook stands aside for that
// build rather than layering: the real signature replaces every nested one
// anyway, so an ad-hoc pass first is minutes of `codesign --deep` thrown away.
// Local and pull-request builds keep `identity: null`, so they always land here.

const { execFileSync } = require("node:child_process");
const path = require("node:path");

// True when electron-builder has a signing identity configured — i.e. when
// `mac.identity` is anything but the `null` the config file sets. `undefined`
// counts as "configured" too: it means auto-discovery, which would sign with
// whatever the keychain holds, and the ad-hoc pass is not what decides that.
function packagerWillSign(platformOptions) {
  return platformOptions?.identity !== null;
}

exports.packagerWillSign = packagerWillSign;

exports.default = async function adhocSign(context) {
  if (context.electronPlatformName !== "darwin") return;
  if (packagerWillSign(context.packager.platformSpecificBuildOptions)) {
    console.log("  • mac.identity is set — skipping the ad-hoc pass");
    return;
  }

  const appPath = path.join(
    context.appOutDir,
    `${context.packager.appInfo.productFilename}.app`,
  );

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
