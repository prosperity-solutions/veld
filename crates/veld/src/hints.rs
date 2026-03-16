use std::path::PathBuf;

fn hints_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".veld").join("hints.json"))
}

/// Show the "setup privileged" hint if appropriate.
/// Returns true if a hint was shown.
pub fn maybe_show_privileged_hint(https_port: u16) -> bool {
    if https_port == 443 {
        return false; // Already in privileged mode
    }

    // Check if hints are disabled
    if std::env::var("VELD_NO_HINTS").is_ok() {
        return false;
    }

    let path = match hints_path() {
        Some(p) => p,
        None => return false,
    };

    // Read current hint state
    let state: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let count = state
        .get("privileged_hint_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Show hint based on count
    let shown = if count == 0 {
        // First time: full message
        eprintln!();
        eprintln!("  \x1b[1mTip:\x1b[0m URLs use port {https_port} in unprivileged mode.");
        eprintln!(
            "  Run \x1b[36mveld setup privileged\x1b[0m for clean URLs without :{https_port} (one-time sudo)."
        );
        eprintln!("  Set VELD_NO_HINTS=1 to silence this message.");
        true
    } else if count < 5 {
        // Brief reminder
        eprintln!();
        eprintln!("  Tip: \x1b[36mveld setup privileged\x1b[0m for URLs without :{https_port}");
        true
    } else {
        false
    };

    // Update count
    if shown {
        let new_state = serde_json::json!({"privileged_hint_count": count + 1});
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, serde_json::to_string(&new_state).unwrap_or_default());
    }

    shown
}
