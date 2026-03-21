use std::path::PathBuf;

/// Resolve the veld lib directory. Prefers `~/.local/lib/veld` (user-level);
/// falls back to `/usr/local/lib/veld` for existing system installs.
///
/// Never attempts to create the system directory — the user-local path is
/// always the default for new installations.
pub fn lib_dir() -> PathBuf {
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
