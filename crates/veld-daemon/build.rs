use std::env;
use std::path::Path;
use std::process::Command;

/// Ensure a package's npm deps are present **and match its lockfile**.
///
/// Fresh checkouts have no node_modules, which would make the build step fail
/// with a cryptic "Cannot find package". Presence alone is not enough, though:
/// npm records the tree it installed in `node_modules/.package-lock.json`, and a
/// checkout that predates a *new dependency* has a complete-looking node_modules
/// that is missing it — so `git pull && cargo build` failed with the same cryptic
/// error, on the branch that added the dependency, for everyone who already had
/// the directory. Comparing the two lockfiles catches that; `npm ci` is
/// idempotent, so a false positive costs one reinstall.
fn ensure_node_modules(dir: &Path) {
    let modules = dir.join("node_modules");
    // npm's own staleness signal: it writes `node_modules/.package-lock.json`
    // after installing, so a `package-lock.json` newer than that marker means the
    // tree on disk was built from a different lockfile. Mtimes rather than a JSON
    // diff because the two documents differ by design (the marker has no
    // name/version header), and because this runs on every build — the cost of a
    // false positive is one idempotent `npm ci`, the cost of a false negative is
    // the cryptic "Cannot find package" this function exists to prevent.
    let stale = match (
        dir.join("package-lock.json")
            .metadata()
            .and_then(|m| m.modified()),
        modules
            .join(".package-lock.json")
            .metadata()
            .and_then(|m| m.modified()),
    ) {
        (Ok(lock), Ok(installed)) => lock > installed,
        // No marker (or no lockfile to compare): fall back to presence alone.
        _ => !modules.exists(),
    };
    if modules.exists() && !stale {
        return;
    }
    let install = Command::new("npm")
        .arg("ci")
        .current_dir(dir)
        .status()
        .expect("failed to run `npm ci` — is Node.js installed?");
    if !install.success() {
        panic!(
            "`npm ci` failed in {} (exit code {:?})",
            dir.display(),
            install.code()
        );
    }
}

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let frontend_dir = Path::new(&manifest_dir).join("frontend");
    let ui_dir = Path::new(&manifest_dir).join("ui");

    // Re-run if any TypeScript source changes.
    println!("cargo::rerun-if-changed=frontend/src");
    println!("cargo::rerun-if-changed=frontend/build.mjs");
    println!("cargo::rerun-if-changed=frontend/package.json");
    println!("cargo::rerun-if-changed=frontend/package-lock.json");
    println!("cargo::rerun-if-changed=ui/src");
    println!("cargo::rerun-if-changed=ui/index.html");
    println!("cargo::rerun-if-changed=ui/vite.config.ts");
    println!("cargo::rerun-if-changed=ui/package.json");
    println!("cargo::rerun-if-changed=ui/package-lock.json");

    // Feedback-overlay / client-log assets: esbuild bundles TS → IIFE JS
    // directly into OUT_DIR.
    ensure_node_modules(&frontend_dir);
    let status = Command::new("npm")
        .arg("run")
        .arg("build")
        .arg("--")
        .arg("--outdir")
        .arg(&out_dir)
        .current_dir(&frontend_dir)
        .status()
        .expect("failed to run `npm run build` — is Node.js installed?");
    if !status.success() {
        panic!("frontend build failed (exit code {:?})", status.code());
    }

    // Management UI v2: vite builds a single self-contained HTML file
    // (JS/CSS/fonts inlined) that the daemon embeds and serves at /ide.
    ensure_node_modules(&ui_dir);
    let status = Command::new("npm")
        .arg("run")
        .arg("build")
        .arg("--")
        .arg("--outDir")
        .arg(&out_dir)
        // No --emptyOutDir: OUT_DIR also holds the esbuild outputs above, and
        // vite leaves out-of-root outDirs alone by default.
        .current_dir(&ui_dir)
        .status()
        .expect("failed to run `npm run build` in ui — is Node.js installed?");
    if !status.success() {
        panic!("management-ui build failed (exit code {:?})", status.code());
    }
    std::fs::rename(
        Path::new(&out_dir).join("index.html"),
        Path::new(&out_dir).join("management-ui-ide.html"),
    )
    .expect("vite build did not produce index.html");
    // The embed is include_str! of that ONE file — if the singlefile plugin
    // ever fails to inline something, vite emits an assets/ dir and the
    // served page would silently load with missing JS/CSS. Fail the build
    // instead.
    let leftover = Path::new(&out_dir).join("assets");
    assert!(
        !leftover.exists(),
        "vite emitted un-inlined assets in {} — the single-file embed would \
         be broken (check vite-plugin-singlefile / assetsInlineLimit)",
        leftover.display()
    );
}
