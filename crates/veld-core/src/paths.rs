use std::path::PathBuf;

/// Resolve the veld lib directory. Prefers `/usr/local/lib/veld` if it exists
/// or is writable; falls back to `~/.local/lib/veld`.
pub fn lib_dir() -> PathBuf {
    let system_dir = PathBuf::from("/usr/local/lib/veld");
    // Use system dir if it already exists (previous install).
    if system_dir.exists() {
        return system_dir;
    }
    // Also check the user-local dir — if it exists, prefer it.
    let user_dir = dirs::home_dir().map(|h| h.join(".local").join("lib").join("veld"));
    if let Some(ref ud) = user_dir {
        if ud.exists() {
            return ud.clone();
        }
    }
    // Neither exists yet (fresh install). Try to create the system dir to test
    // writability, then clean up.
    if std::fs::create_dir_all(&system_dir).is_ok() {
        return system_dir;
    }
    // Fall back to user-local dir.
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
