//! Instance paths and nonsecret configuration for explicitly installed runners.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_DAEMON_PORT: u16 = 19895;
pub const DEFAULT_REMOTE_PORT: u16 = 19896;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    pub endpoint: String,
    pub remote_port: u16,
    pub daemon_port: u16,
}

/// A runner uses independent state rather than migrating an installed daemon's database.
pub fn database_for_name(name: &str) -> Result<PathBuf> {
    if name.is_empty()
        || name.len() > 40
        || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        bail!("runner name must contain 1–40 letters, digits, or hyphens");
    }
    Ok(dirs::data_local_dir()
        .context("cannot locate the user data directory")?
        .join("veld/runners")
        .join(name)
        .join("veld.db"))
}

pub fn state_directory(database: &Path) -> PathBuf {
    database.with_extension("remote")
}

/// Keep Unix socket names below platform limits, even for deeply nested worktrees.
pub fn admin_socket(database: &Path) -> Result<PathBuf> {
    let digest = Sha256::digest(database.as_os_str().as_encoded_bytes());
    let key: String = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(dirs::home_dir()
        .context("cannot locate the user home directory")?
        .join(".veld")
        .join(format!("remote-{key}.sock")))
}

pub fn config_path(database: &Path) -> PathBuf {
    state_directory(database).join("config.json")
}

/// Retained holders belong to a named runner, independently of reusable TCP ports.
pub fn pty_directory(database: &Path) -> Result<PathBuf> {
    Ok(admin_socket(database)?.with_extension("pty"))
}

pub fn load_config(database: &Path) -> Result<Option<RunnerConfig>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let path = config_path(database);
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("cannot read runner configuration"),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        bail!("runner configuration must be a private file owned by this user");
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        bail!("runner configuration exceeds the size limit");
    }
    let config: RunnerConfig =
        serde_json::from_slice(&bytes).context("invalid runner configuration")?;
    if config.daemon_port == 0
        || config.remote_port == 0
        || config.daemon_port == config.remote_port
    {
        bail!("invalid runner listener ports");
    }
    let canonical = crate::remote::canonical_endpoint(
        &config.endpoint,
        crate::remote::EndpointPolicy::Production,
    )?;
    if canonical != config.endpoint {
        bail!("runner endpoint is not canonical");
    }
    Ok(Some(config))
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdminRequest {
    Status,
    Pair,
    Clients,
    Revoke { client_id: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cannot_escape_the_runner_directory() {
        for name in ["", "../other", "a/b", "a.b", "with space"] {
            assert!(database_for_name(name).is_err());
        }
        assert!(
            database_for_name("linux-trial")
                .unwrap()
                .ends_with("linux-trial/veld.db")
        );
    }

    #[test]
    fn sockets_are_short_and_instance_specific() {
        let first = admin_socket(Path::new("/very/long/worktree/a/veld.db")).unwrap();
        let second = admin_socket(Path::new("/very/long/worktree/b/veld.db")).unwrap();
        assert_ne!(first, second);
        assert!(first.as_os_str().len() < 104);
    }
}
