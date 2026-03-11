use crate::output;
use std::path::Path;
use std::process::Command;

const HELPER_PATH: &str = "/usr/local/lib/veld/veld-helper";
const DAEMON_PATH: &str = "/usr/local/lib/veld/veld-daemon";

/// Print version information for all Veld binaries.
pub fn print_version() {
    let cli_version = env!("CARGO_PKG_VERSION");

    // The daemon and helper binaries share the workspace version. When those
    // crates expose a `VERSION` constant we can read it directly; for now we
    // use the same workspace version.
    let daemon_version = cli_version;
    let helper_version = cli_version;

    println!("{}", output::bold("Veld"));
    println!("  cli      {cli_version}");
    println!("  daemon   {daemon_version}");
    println!("  helper   {helper_version}");
}

/// Query a binary's version by running `<path> --version` and extracting the
/// version string. Returns `None` if the binary doesn't exist or we can't
/// parse its output.
fn query_binary_version(path: &str) -> Option<String> {
    if !Path::new(path).exists() {
        return None;
    }

    let output = Command::new(path).arg("--version").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The binary may print something like "veld-helper 0.1.0" or just "0.1.0".
    // Take the last whitespace-delimited token that looks like a version.
    let version = stdout.split_whitespace().last()?.to_string();

    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Check that installed helper and daemon binaries match the CLI version.
/// Returns `Ok(())` if everything is fine, or `Err(message)` with a
/// user-facing error string if there is a mismatch.
pub fn check_version_mismatch() -> Result<(), String> {
    let cli_version = env!("CARGO_PKG_VERSION");
    let mut mismatches: Vec<String> = Vec::new();

    if let Some(v) = query_binary_version(HELPER_PATH) {
        if v != cli_version {
            mismatches.push(format!("veld-helper is v{v} (expected v{cli_version})"));
        }
    }

    if let Some(v) = query_binary_version(DAEMON_PATH) {
        if v != cli_version {
            mismatches.push(format!("veld-daemon is v{v} (expected v{cli_version})"));
        }
    }

    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Version mismatch detected: {}. Run `veld update` to fix this.",
            mismatches.join(", ")
        ))
    }
}
