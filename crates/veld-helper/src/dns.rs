use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{debug, info};

/// Marker comments used to fence managed entries in /etc/hosts.
const HOSTS_BEGIN_MARKER: &str = "# BEGIN veld-managed";
const HOSTS_END_MARKER: &str = "# END veld-managed";

/// In-memory state of DNS entries managed by this helper.
#[derive(Debug)]
pub struct DnsManager {
    inner: Arc<Mutex<DnsState>>,
}

#[derive(Debug)]
struct DnsState {
    /// hostname -> ip
    entries: HashMap<String, String>,
    /// Path to the dnsmasq config file we manage.
    dnsmasq_conf: PathBuf,
}

impl DnsManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DnsState {
                entries: HashMap::new(),
                dnsmasq_conf: veld_core::paths::dnsmasq_conf_dir().join("veld.conf"),
            })),
        }
    }

    /// Add a DNS entry. For `.localhost` domains no file writes are performed
    /// (RFC 6761 guarantees resolution). For other domains we write to the
    /// dnsmasq include file and update /etc/hosts as a fallback.
    pub async fn add_host(&self, hostname: &str, ip: &str) -> Result<()> {
        let mut state = self.inner.lock().await;
        state.entries.insert(hostname.to_string(), ip.to_string());

        if is_localhost_domain(hostname) {
            debug!(
                hostname,
                "skipping DNS write for .localhost domain (RFC 6761)"
            );
            return Ok(());
        }

        // Write dnsmasq config.
        Self::write_dnsmasq_conf(&state).await?;
        // Also update /etc/hosts as a fallback.
        Self::write_hosts_file(&state).await?;

        info!(hostname, ip, "DNS entry added");
        Ok(())
    }

    /// Remove a DNS entry.
    pub async fn remove_host(&self, hostname: &str) -> Result<()> {
        let mut state = self.inner.lock().await;
        state.entries.remove(hostname);

        if is_localhost_domain(hostname) {
            debug!(hostname, "no DNS cleanup needed for .localhost domain");
            return Ok(());
        }

        Self::write_dnsmasq_conf(&state).await?;
        Self::write_hosts_file(&state).await?;

        info!(hostname, "DNS entry removed");
        Ok(())
    }

    /// Return the number of managed entries.
    pub async fn entry_count(&self) -> usize {
        self.inner.lock().await.entries.len()
    }

    // ---- private helpers ---------------------------------------------------

    /// Write the dnsmasq include file with all current non-localhost entries.
    async fn write_dnsmasq_conf(state: &DnsState) -> Result<()> {
        let conf_path = &state.dnsmasq_conf;

        if let Some(parent) = conf_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating dir {}", parent.display()))?;
        }

        let content = build_dnsmasq_content(&state.entries);
        tokio::fs::write(conf_path, content.as_bytes())
            .await
            .with_context(|| format!("writing {}", conf_path.display()))?;

        Ok(())
    }

    /// Rewrite the fenced section inside /etc/hosts.
    async fn write_hosts_file(state: &DnsState) -> Result<()> {
        let hosts_path = Path::new("/etc/hosts");

        let existing = tokio::fs::read_to_string(hosts_path)
            .await
            .unwrap_or_default();

        let new_content = rebuild_hosts_file(&existing, &state.entries);

        tokio::fs::write(hosts_path, new_content.as_bytes())
            .await
            .with_context(|| "writing /etc/hosts".to_string())?;

        Ok(())
    }
}

/// Reload the system DNS resolver / dnsmasq.
pub async fn reload_dns() -> Result<()> {
    if cfg!(target_os = "macos") {
        // Flush the macOS DNS cache.
        let status = tokio::process::Command::new("dscacheutil")
            .arg("-flushcache")
            .status()
            .await
            .context("running dscacheutil -flushcache")?;

        if !status.success() {
            anyhow::bail!("dscacheutil -flushcache exited with {status}");
        }

        // Also poke mDNSResponder via killall.
        let _ = tokio::process::Command::new("killall")
            .args(["-HUP", "mDNSResponder"])
            .status()
            .await;
    } else {
        // On Linux, restart dnsmasq.
        let status = tokio::process::Command::new("systemctl")
            .args(["restart", "dnsmasq"])
            .status()
            .await
            .context("restarting dnsmasq")?;

        if !status.success() {
            anyhow::bail!("systemctl restart dnsmasq exited with {status}");
        }
    }

    info!("DNS reloaded");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_localhost_domain(hostname: &str) -> bool {
    hostname == "localhost" || hostname.ends_with(".localhost")
}

/// Build dnsmasq address directives for all non-localhost entries.
fn build_dnsmasq_content(entries: &HashMap<String, String>) -> String {
    let mut lines: Vec<String> = entries
        .iter()
        .filter(|(h, _)| !is_localhost_domain(h))
        .map(|(h, ip)| format!("address=/{h}/{ip}"))
        .collect();
    lines.sort();
    if lines.is_empty() {
        String::new()
    } else {
        lines.join("\n") + "\n"
    }
}

/// Rebuild /etc/hosts preserving content outside the Veld markers.
fn rebuild_hosts_file(existing: &str, entries: &HashMap<String, String>) -> String {
    let mut before_marker = String::new();
    let mut after_marker = String::new();
    let mut inside = false;
    let mut found_section = false;

    for line in existing.lines() {
        if line.trim() == HOSTS_BEGIN_MARKER {
            inside = true;
            found_section = true;
            continue;
        }
        if line.trim() == HOSTS_END_MARKER {
            inside = false;
            continue;
        }
        if inside {
            continue;
        }
        if found_section {
            after_marker.push_str(line);
            after_marker.push('\n');
        } else {
            before_marker.push_str(line);
            before_marker.push('\n');
        }
    }

    // Build the managed section.
    let mut managed_lines: Vec<String> = entries
        .iter()
        .filter(|(h, _)| !is_localhost_domain(h))
        .map(|(h, ip)| format!("{ip}\t{h}"))
        .collect();
    managed_lines.sort();

    let mut result = before_marker;
    if !managed_lines.is_empty() {
        result.push_str(HOSTS_BEGIN_MARKER);
        result.push('\n');
        for line in &managed_lines {
            result.push_str(line);
            result.push('\n');
        }
        result.push_str(HOSTS_END_MARKER);
        result.push('\n');
    }
    result.push_str(&after_marker);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_localhost_detection() {
        assert!(is_localhost_domain("localhost"));
        assert!(is_localhost_domain("app.test.localhost"));
        assert!(!is_localhost_domain("app.test.dev"));
    }

    #[test]
    fn test_rebuild_hosts_empty() {
        let entries = HashMap::new();
        let result = rebuild_hosts_file("127.0.0.1 localhost\n", &entries);
        assert_eq!(result, "127.0.0.1 localhost\n");
    }

    #[test]
    fn test_rebuild_hosts_with_entries() {
        let mut entries = HashMap::new();
        entries.insert("myapp.dev".to_string(), "127.0.0.1".to_string());

        let existing = "127.0.0.1 localhost\n";
        let result = rebuild_hosts_file(existing, &entries);
        assert!(result.contains(HOSTS_BEGIN_MARKER));
        assert!(result.contains("127.0.0.1\tmyapp.dev"));
        assert!(result.contains(HOSTS_END_MARKER));
    }

    #[test]
    fn test_dnsmasq_content() {
        let mut entries = HashMap::new();
        entries.insert("myapp.dev".to_string(), "127.0.0.1".to_string());
        entries.insert("api.app.localhost".to_string(), "127.0.0.1".to_string());

        let content = build_dnsmasq_content(&entries);
        assert!(content.contains("address=/myapp.dev/127.0.0.1"));
        // .localhost entries should be excluded.
        assert!(!content.contains("app.localhost"));
    }
}
