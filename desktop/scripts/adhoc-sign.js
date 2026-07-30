// electron-builder `afterPack` hook: ad-hoc sign the macOS bundle.
//
// Not a substitute for Developer ID signing (issue #167 §10) — an ad-hoc
// signature carries no identity, so Gatekeeper still quarantines the download
// and the user still has to open it explicitly the first time. What it fixes is
// worse than a warning: on Apple Silicon every executable must carry *some*
// valid signature to run at all, and repacking Electron (asar, rename, icon)
// invalidates the one the prebuilt binaries came with. An unsigned bundle there
// does not warn — it reports "Veld is damaged and can't be opened", which reads
// as a corrupt download and has no in-UI way out.
//
// Runs instead of electron-builder's signing, which `mac.identity: null` turns
// off. When a Developer ID lands, this hook goes away rather than layering: a
// real signature must be applied by the packager (it also drives notarization),
// and signing twice would just discard the first one.

const { execFileSync } = require("node:child_process");
const path = require("node:path");

exports.default = async function adhocSign(context) {
  if (context.electronPlatformName !== "darwin") return;

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
