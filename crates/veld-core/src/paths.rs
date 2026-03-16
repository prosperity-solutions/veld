use std::path::PathBuf;

/// Resolve the veld lib directory. Prefers `/usr/local/lib/veld` if it exists
/// or can be created; falls back to `~/.local/lib/veld`.
///
/// On fresh installs, tries to create the system dir to test writability.
/// This avoids HOME-dependent resolution that differs between processes
/// (e.g. sudo'd setup vs LaunchDaemon).
pub fn lib_dir() -> PathBuf {
    let system_dir = PathBuf::from("/usr/local/lib/veld");
    // Use system dir if it already exists.
    if system_dir.exists() {
        return system_dir;
    }
    // Check user-local dir.
    let user_dir = dirs::home_dir().map(|h| h.join(".local").join("lib").join("veld"));
    if let Some(ref ud) = user_dir {
        if ud.exists() {
            return ud.clone();
        }
    }
    // Neither exists yet. Try to create the system dir. If we can, use it.
    // This ensures all processes (setup, helper daemon, CLI) agree on the
    // same path regardless of HOME.
    if std::fs::create_dir_all(&system_dir).is_ok() {
        return system_dir;
    }
    // Fall back to user-local dir (non-root install).
    user_dir.unwrap_or(system_dir)
}

pub fn caddy_bin() -> PathBuf {
    lib_dir().join("caddy")
}

pub fn caddy_data_dir() -> PathBuf {
    lib_dir().join("caddy-data")
}

/// Marker file that records the download URL used for the current Caddy binary.
/// Used to detect when the binary needs upgrading (e.g. new plugins).
pub fn caddy_url_marker() -> PathBuf {
    lib_dir().join(".caddy-url")
}

pub fn dnsmasq_conf_dir() -> PathBuf {
    lib_dir().join("dnsmasq.d")
}
