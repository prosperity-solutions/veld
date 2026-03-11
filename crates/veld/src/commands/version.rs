use crate::output;
use std::path::Path;
use std::process::Command;

/// Print version information for all Veld binaries.
pub fn print_version() {
    let cli_version = env!("CARGO_PKG_VERSION");

    let helper_version =
        find_and_query_version("veld-helper").unwrap_or_else(|| cli_version.to_string());
    let daemon_version =
        find_and_query_version("veld-daemon").unwrap_or_else(|| cli_version.to_string());

    println!("{}", output::bold("Veld"));
    println!("  cli      {cli_version}");
    println!("  daemon   {daemon_version}");
    println!("  helper   {helper_version}");
}

/// Find a binary by checking known paths and query its version.
fn find_and_query_version(binary_name: &str) -> Option<String> {
    let candidates = binary_candidates(binary_name);
    for path in &candidates {
        if let Some(v) = query_binary_version(path) {
            return Some(v);
        }
    }
    None
}

/// Build list of candidate paths for a binary.
fn binary_candidates(binary_name: &str) -> Vec<String> {
    let mut paths = vec![format!("/usr/local/lib/veld/{binary_name}")];
    if let Some(home) = dirs::home_dir() {
        paths.push(
            home.join(".local")
                .join("lib")
                .join("veld")
                .join(binary_name)
                .to_string_lossy()
                .into_owned(),
        );
    }
    paths
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
    // The binary prints "veld-helper 1.0.0" — take the last token.
    let version = stdout.split_whitespace().last()?.to_string();

    // Sanity check: version should contain a dot (e.g. "1.0.0").
    if version.contains('.') {
        Some(version)
    } else {
        None
    }
}

/// Check that installed helper and daemon binaries match the CLI version.
/// Returns `Ok(())` if everything is fine, or `Err(message)` with a
/// user-facing error string if there is a mismatch.
pub fn check_version_mismatch() -> Result<(), String> {
    let cli_version = env!("CARGO_PKG_VERSION");
    let mut mismatches: Vec<String> = Vec::new();

    if let Some(v) = find_and_query_version("veld-helper") {
        if v != cli_version {
            mismatches.push(format!("veld-helper is v{v} (expected v{cli_version})"));
        }
    }

    if let Some(v) = find_and_query_version("veld-daemon") {
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
