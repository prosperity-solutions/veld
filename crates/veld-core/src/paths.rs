use std::path::PathBuf;

/// Resolve the veld lib directory.
///
/// Resolution order:
/// 1. `VELD_LIB_DIR` env var (for local dev — points at `target/debug/`)
/// 2. `~/.local/lib/veld` (user-level, default for new installs)
/// 3. `/usr/local/lib/veld` (legacy system installs)
pub fn lib_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("VELD_LIB_DIR") {
        return PathBuf::from(dir);
    }
    // Prefer user-local directory (new default).
    let user_dir = dirs::home_dir().map(|h| h.join(".local").join("lib").join("veld"));
    if let Some(ref ud) = user_dir {
        if ud.exists() {
            return ud.clone();
        }
    }
    // Fall back to system directory for existing installs.
    let system_dir = PathBuf::from("/usr/local/lib/veld");
    if system_dir.exists() {
        return system_dir;
    }
    // Default to user-local directory (never try to create system dir).
    user_dir.unwrap_or(system_dir)
}

pub fn caddy_bin() -> PathBuf {
    lib_dir().join("caddy")
}

pub fn caddy_data_dir() -> PathBuf {
    lib_dir().join("caddy-data")
}

pub fn dnsmasq_conf_dir() -> PathBuf {
    lib_dir().join("dnsmasq.d")
}
