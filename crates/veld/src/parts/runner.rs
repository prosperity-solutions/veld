//! Headless runner installation and owner-only pairing administration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use veld_core::runner::{AdminRequest, RunnerConfig};

#[derive(Subcommand)]
pub enum RunnerCmd {
    /// Install an isolated Linux user service and publish its authenticated endpoint over Tailscale.
    Install {
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long, default_value_t = veld_core::runner::DEFAULT_DAEMON_PORT)]
        daemon_port: u16,
        #[arg(long, default_value_t = veld_core::runner::DEFAULT_REMOTE_PORT)]
        remote_port: u16,
    },
    /// Run an isolated runner in this terminal. HTTPS publication is configured separately.
    Serve {
        #[arg(long)]
        endpoint: String,
        #[arg(long, default_value_t = veld_core::runner::DEFAULT_DAEMON_PORT)]
        daemon_port: u16,
        #[arg(long, default_value_t = veld_core::runner::DEFAULT_REMOTE_PORT)]
        remote_port: u16,
    },
    /// Show this runner's identity and endpoint.
    Status,
    /// Issue one invitation, valid for five minutes. Keep the output private.
    Pair,
    /// List paired clients without credentials.
    Clients,
    /// Revoke one client and close its connections without stopping host work.
    Revoke { client_id: String },
    /// Remove this runner's service and Tailscale mapping, retaining its private state.
    Uninstall,
}

pub async fn run(name: String, command: RunnerCmd) -> i32 {
    match execute(&name, command).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Runner: {error:#}");
            1
        }
    }
}

async fn execute(name: &str, command: RunnerCmd) -> Result<()> {
    let database = veld_core::runner::database_for_name(name)?;
    match command {
        RunnerCmd::Install {
            endpoint,
            daemon_port,
            remote_port,
        } => install(name, &database, endpoint, daemon_port, remote_port).await,
        RunnerCmd::Serve {
            endpoint,
            daemon_port,
            remote_port,
        } => {
            prepare(&database, &endpoint, daemon_port, remote_port)?;
            let status = daemon(&database, daemon_port)?.status().await?;
            if !status.success() {
                bail!("runner daemon exited with {status}");
            }
            Ok(())
        }
        RunnerCmd::Uninstall => uninstall(name, &database).await,
        other => {
            let request = match other {
                RunnerCmd::Status => AdminRequest::Status,
                RunnerCmd::Pair => AdminRequest::Pair,
                RunnerCmd::Clients => AdminRequest::Clients,
                RunnerCmd::Revoke { client_id } => AdminRequest::Revoke { client_id },
                _ => unreachable!(),
            };
            let response = admin(&database, request).await?;
            if let Some(invitation) = response.get("invitation").and_then(|v| v.as_str()) {
                eprintln!(
                    "Paste this invitation into Veld on the other machine. It grants control as this OS user."
                );
                println!("{invitation}");
            } else {
                println!("{}", serde_json::to_string_pretty(&response)?);
            }
            Ok(())
        }
    }
}

fn daemon_binary() -> Result<PathBuf> {
    let current = std::env::current_exe()?;
    let sibling = current.with_file_name("veld-daemon");
    if !sibling.is_file() {
        bail!(
            "matching veld-daemon is missing beside {}",
            current.display()
        );
    }
    Ok(sibling)
}

fn daemon(database: &Path, port: u16) -> Result<Command> {
    let mut command = Command::new(daemon_binary()?);
    command
        .env("VELD_DB_PATH", database)
        .env("VELD_DAEMON_PORT", port.to_string())
        .env(
            "VELD_DAEMON_SOCK",
            veld_core::runner::admin_socket(database)?.with_extension("daemon.sock"),
        )
        .env("VELD_PTY_DIR", veld_core::runner::pty_directory(database)?)
        .env_remove("VELD_MANAGEMENT_HOST")
        .env_remove("VELD_PROXY_ORIGINS")
        .env_remove("VELD_URL")
        .env_remove("VELD_PORT");
    Ok(command)
}

fn prepare(database: &Path, endpoint: &str, daemon_port: u16, remote_port: u16) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if daemon_port == 0
        || remote_port == 0
        || daemon_port == remote_port
        || daemon_port == veld_core::instance::DEFAULT_DAEMON_PORT
        || remote_port == veld_core::instance::DEFAULT_DAEMON_PORT
    {
        bail!("runner ports must be distinct, nonzero, and separate from the installed daemon");
    }
    let parent = database
        .parent()
        .context("runner database needs a parent")?;
    std::fs::create_dir_all(parent)?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("runner state directory must belong to this user");
    }
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    // Opening validates the endpoint and initializes only this runner's private identity.
    let state = veld_core::runner::state_directory(database);
    let store = veld_core::remote::RunnerStore::open(&state, endpoint)?;
    let canonical = store.descriptor().endpoint.clone();
    let config = RunnerConfig {
        endpoint: canonical,
        remote_port,
        daemon_port,
    };
    if let Some(existing) = veld_core::runner::load_config(database)? {
        if existing.endpoint != config.endpoint
            || existing.remote_port != config.remote_port
            || existing.daemon_port != config.daemon_port
        {
            bail!(
                "runner configuration already exists with another endpoint; choose another runner name"
            );
        }
        return Ok(());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&state)?;
    use std::io::Write;
    temporary.write_all(&serde_json::to_vec_pretty(&config)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(veld_core::runner::config_path(database))?;
    Ok(())
}

async fn admin(database: &Path, request: AdminRequest) -> Result<serde_json::Value> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut stream =
            tokio::net::UnixStream::connect(veld_core::runner::admin_socket(database)?)
                .await
                .context("runner is not available; install or start it first")?;
        stream.write_all(&serde_json::to_vec(&request)?).await?;
        stream.write_all(b"\n").await?;
        let mut reader = BufReader::new(stream);
        let mut line = Vec::new();
        loop {
            let bytes = reader.fill_buf().await?;
            if bytes.is_empty() {
                bail!("incomplete runner response");
            }
            let count = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |n| n + 1);
            if line.len() + count > 1024 * 1024 {
                bail!("runner response exceeds the size limit");
            }
            line.extend_from_slice(&bytes[..count]);
            reader.consume(count);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        let response: serde_json::Value = serde_json::from_slice(&line)?;
        if response.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            bail!(
                "{}",
                response
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("runner administration failed")
            );
        }
        Ok(response["result"].clone())
    })
    .await
    .context("runner administration timed out")?
}

async fn checked(program: &str, args: &[&str]) -> Result<std::process::Output> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await
    .with_context(|| format!("{program} timed out"))??;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

async fn install(
    name: &str,
    database: &Path,
    endpoint: Option<String>,
    daemon_port: u16,
    remote_port: u16,
) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(
            "runner service installation currently supports Linux; use runner serve for a foreground trial"
        );
    }
    let uid = unsafe { libc::geteuid() }.to_string();
    let lingering = checked(
        "loginctl",
        &["show-user", &uid, "--property=Linger", "--value"],
    )
    .await?;
    if String::from_utf8_lossy(&lingering.stdout).trim() != "yes" {
        bail!(
            "enable user lingering through the machine's administrator before installing an unattended runner"
        );
    }
    let tailscale = checked("tailscale", &["status", "--json"]).await?;
    let status: serde_json::Value = serde_json::from_slice(&tailscale.stdout)?;
    if status["BackendState"].as_str() != Some("Running") {
        bail!("sign in to Tailscale on this machine first");
    }
    let dns = status["Self"]["DNSName"]
        .as_str()
        .context("Tailscale did not report a DNS name")?
        .trim_end_matches('.');
    let expected = format!("https://{dns}:{remote_port}");
    let endpoint = endpoint.unwrap_or_else(|| expected.clone());
    if endpoint.trim_end_matches('/') != expected {
        bail!("runner endpoint must match this machine's Tailscale DNS name and remote port");
    }
    prepare(database, &endpoint, daemon_port, remote_port)?;
    let binary = daemon_binary()?;
    let unit_name = format!("veld-runner-{name}.service");
    let units = dirs::config_dir()
        .context("cannot locate the user configuration directory")?
        .join("systemd/user");
    std::fs::create_dir_all(&units)?;
    let unit = service_unit(&binary, database, daemon_port)?;
    let unit_path = units.join(&unit_name);
    match std::fs::read_to_string(&unit_path) {
        Ok(existing) if existing != unit => {
            bail!("service already exists with different settings; choose another runner name")
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(&unit_path, unit)?
        }
        Err(error) => return Err(error).context("cannot inspect runner service"),
    }
    checked("systemctl", &["--user", "daemon-reload"]).await?;
    checked("systemctl", &["--user", "enable", "--now", &unit_name]).await?;
    let descriptor = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(status) = admin(database, AdminRequest::Status).await {
                let descriptor: veld_core::remote::Descriptor =
                    serde_json::from_value(status["descriptor"].clone())?;
                let expected = veld_core::remote::RunnerStore::open(
                    &veld_core::runner::state_directory(database),
                    &endpoint,
                )?
                .descriptor();
                if descriptor != expected
                    || status["upstream"].as_str()
                        != Some(&format!("http://127.0.0.1:{daemon_port}"))
                {
                    bail!("running service does not match this runner's identity and listener");
                }
                return Ok::<_, anyhow::Error>(descriptor);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .context("runner readiness timed out; inspect its user service journal")??;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let health: serde_json::Value = client
        .get(format!("http://127.0.0.1:{daemon_port}/api/health"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if health["db_path"].as_str() != database.to_str() {
        bail!("runner API uses an unexpected database");
    }
    checked("systemctl", &["--user", "is-active", &unit_name]).await?;
    checked("systemctl", &["--user", "is-enabled", &unit_name]).await?;
    publish(remote_port, dns).await?;
    let nonce = veld_core::remote::generate_nonce();
    let response = client
        .get(format!("{endpoint}/remote/descriptor"))
        .query(&[("nonce", &nonce)])
        .send()
        .await?
        .error_for_status()?;
    let proof: veld_core::remote::IdentityProof = response.json().await?;
    veld_core::remote::verify_proof(&proof, &descriptor, &nonce)?;
    println!("Runner {name} is available at {endpoint}. Pairing is closed.");
    println!("Run: veld runner --name {name} pair");
    Ok(())
}

async fn uninstall(name: &str, database: &Path) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("runner service removal currently supports Linux");
    }
    let config = veld_core::runner::load_config(database)?.context("runner is not installed")?;
    let unit_name = format!("veld-runner-{name}.service");
    let unit_path = dirs::config_dir()
        .context("cannot locate the user configuration directory")?
        .join("systemd/user")
        .join(&unit_name);
    let expected = service_unit(&daemon_binary()?, database, config.daemon_port)?;
    if std::fs::read_to_string(&unit_path)? != expected {
        bail!("service definition changed; refusing to remove another instance");
    }
    let output = checked("tailscale", &["serve", "status", "--json"]).await?;
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let authority = config
        .endpoint
        .strip_prefix("https://")
        .context("invalid runner endpoint")?;
    let upstream = format!("http://127.0.0.1:{}", config.remote_port);
    if status["TCP"].get(config.remote_port.to_string()).is_some() {
        if status["Web"][authority]["Handlers"]["/"]["Proxy"].as_str() != Some(&upstream)
            || status["Web"][authority]["Handlers"]
                .as_object()
                .is_none_or(|handlers| handlers.len() != 1)
        {
            bail!("Tailscale mapping changed; refusing to remove another service");
        }
        checked(
            "tailscale",
            &["serve", &format!("--https={}", config.remote_port), "off"],
        )
        .await?;
    }
    checked("systemctl", &["--user", "disable", "--now", &unit_name]).await?;
    std::fs::remove_file(unit_path)?;
    checked("systemctl", &["--user", "daemon-reload"]).await?;
    println!("Removed runner {name}. Private state and retained host jobs remain.");
    Ok(())
}

fn unit_value(value: &str) -> Result<String> {
    if value.contains(['\n', '\r', '\0']) {
        bail!("service path contains a control character");
    }
    Ok(value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%"))
}

fn service_unit(binary: &Path, database: &Path, daemon_port: u16) -> Result<String> {
    let socket = veld_core::runner::admin_socket(database)?.with_extension("daemon.sock");
    Ok(format!(
        "[Unit]\nDescription=Veld headless runner\nAfter=network.target\n\n[Service]\nExecStart=\"{}\"\nEnvironment=\"VELD_DB_PATH={}\" \"VELD_DAEMON_PORT={daemon_port}\" \"VELD_DAEMON_SOCK={}\" \"VELD_PTY_DIR={}\"\nUnsetEnvironment=VELD_MANAGEMENT_HOST VELD_PROXY_ORIGINS VELD_URL VELD_PORT\nRestart=on-failure\nKillMode=process\n\n[Install]\nWantedBy=default.target\n",
        unit_value(&binary.to_string_lossy())?,
        unit_value(&database.to_string_lossy())?,
        unit_value(&socket.to_string_lossy())?,
        unit_value(&veld_core::runner::pty_directory(database)?.to_string_lossy())?
    ))
}

async fn publish(port: u16, dns: &str) -> Result<()> {
    let output = checked("tailscale", &["serve", "status", "--json"]).await?;
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let authority = format!("{dns}:{port}");
    let upstream = format!("http://127.0.0.1:{port}");
    if status["AllowFunnel"][&authority].as_bool() == Some(true) {
        bail!("the runner port is already public through Funnel; choose another port");
    }
    if status["TCP"].get(port.to_string()).is_some() {
        if status["Web"][&authority]["Handlers"]["/"]["Proxy"].as_str() == Some(&upstream) {
            return Ok(());
        }
        bail!("the Tailscale HTTPS port already belongs to another mapping; choose another port");
    }
    checked(
        "tailscale",
        &["serve", "--bg", &format!("--https={port}"), &upstream],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_values_do_not_expand_systemd_specifiers() {
        assert_eq!(
            unit_value("/a%u/\"quoted\"").unwrap(),
            "/a%%u/\\\"quoted\\\""
        );
        assert!(unit_value("bad\nExecStart=/bin/false").is_err());
    }

    #[test]
    fn service_is_isolated_and_preserves_worker_processes() {
        let unit = service_unit(
            Path::new("/opt/veld/veld-daemon"),
            Path::new("/tmp/trial/veld.db"),
            19895,
        )
        .unwrap();
        assert!(unit.contains("VELD_DB_PATH=/tmp/trial/veld.db"));
        assert!(unit.contains("KillMode=process"));
        assert!(unit.contains("VELD_PTY_DIR="));
        let other = service_unit(
            Path::new("/opt/veld/veld-daemon"),
            Path::new("/tmp/other/veld.db"),
            19895,
        )
        .unwrap();
        let pty = veld_core::runner::pty_directory(Path::new("/tmp/trial/veld.db")).unwrap();
        assert!(!other.contains(pty.to_str().unwrap()));
        assert!(!unit.contains("User=root"));
    }
}
