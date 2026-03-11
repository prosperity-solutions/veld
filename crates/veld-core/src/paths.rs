use std::path::PathBuf;

/// Resolve the veld lib directory. Prefers `/usr/local/lib/veld` if it exists;
/// falls back to `~/.local/lib/veld`. If neither exists, checks whether
/// `/usr/local/lib/` is writable to decide where a fresh install should go.
pub fn lib_dir() -> PathBuf {
    let system_dir = PathBuf::from("/usr/local/lib/veld");
    if system_dir.exists() {
        return system_dir;
    }
    let user_dir = dirs::home_dir().map(|h| h.join(".local").join("lib").join("veld"));
    if let Some(ref ud) = user_dir {
        if ud.exists() {
            return ud.clone();
        }
    }
    // Neither exists yet. Check if the parent of the system dir is writable
    // without creating anything.
    let parent = PathBuf::from("/usr/local/lib");
    if parent.exists() && is_dir_writable(&parent) {
        return system_dir;
    }
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

/// Check if a directory is writable by attempting to create and remove a temp file.
fn is_dir_writable(path: &std::path::Path) -> bool {
    let probe = path.join(".veld-probe");
    if std::fs::write(&probe, b"").is_ok() {
        let _ = std::fs::remove_file(&probe);
        true
    } else {
        false
    }
}
