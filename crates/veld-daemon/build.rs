use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let frontend_dir = Path::new(&manifest_dir).join("frontend");

    // Re-run if any TypeScript source changes.
    println!("cargo::rerun-if-changed=frontend/src");
    println!("cargo::rerun-if-changed=frontend/build.mjs");
    println!("cargo::rerun-if-changed=frontend/package.json");

    // Run esbuild via npm to bundle + minify TypeScript → JS/CSS.
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
}
