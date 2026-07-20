use std::env;
use std::path::Path;
use std::process::Command;

/// Ensure a package's npm deps are present. Fresh checkouts have no
/// node_modules, which would make the build step fail with a cryptic
/// "Cannot find package". Install them once if missing.
fn ensure_node_modules(dir: &Path) {
    if dir.join("node_modules").exists() {
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
    // (JS/CSS/fonts inlined) that the daemon embeds and serves at /v2.
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
        Path::new(&out_dir).join("management-ui-v2.html"),
    )
    .expect("vite build did not produce index.html");
}
