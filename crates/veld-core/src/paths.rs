use std::path::PathBuf;

/// Resolve the veld lib directory. Prefers `/usr/local/lib/veld` if it exists
/// or is writable; falls back to `~/.local/lib/veld`.
pub fn lib_dir() -> PathBuf {
    let system_dir = PathBuf::from("/usr/local/lib/veld");
    // Use system dir if it already exists (previous install).
    if system_dir.exists() {
        return system_dir;
    }
    // Use system dir if parent is writable (fresh install with permissions).
    if let Some(parent) = system_dir.parent() {
        if parent.exists()
            && std::fs::metadata(parent)
                .map(|m| !m.permissions().readonly())
                .unwrap_or(false)
        {
            return system_dir;
        }
    }
    // Fall back to user-local dir.
    dirs::home_dir()
        .map(|h| h.join(".local").join("lib").join("veld"))
        .unwrap_or(system_dir)
}

pub fn caddy_bin() -> PathBuf {
    lib_dir().join("caddy")
}

pub fn mkcert_bin() -> PathBuf {
    lib_dir().join("mkcert")
}

pub fn certs_dir() -> PathBuf {
    lib_dir().join("certs")
}

pub fn caddy_data_dir() -> PathBuf {
    lib_dir().join("caddy-data")
}

pub fn dnsmasq_conf_dir() -> PathBuf {
    lib_dir().join("dnsmasq.d")
}
