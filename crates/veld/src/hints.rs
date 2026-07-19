const KV_HINT_COUNT: &str = "hints.privileged_hint_count";

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

    let Ok(db) = veld_core::db::Db::open() else {
        return false;
    };

    let count: u64 = db
        .kv_get(KV_HINT_COUNT)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
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
        let _ = db.kv_set(KV_HINT_COUNT, &(count + 1).to_string());
    }

    shown
}
