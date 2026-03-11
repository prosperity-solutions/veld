use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to spawn process: {0}")]
    SpawnFailed(#[source] std::io::Error),

    #[error("process exited with code {0}")]
    NonZeroExit(i32),

    #[error("process was killed by signal")]
    Signaled,

    #[error("failed to send signal to pid {pid}: {source}")]
    SignalFailed { pid: u32, source: std::io::Error },
}

// ---------------------------------------------------------------------------
// Parsed output from a bash step
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct BashOutput {
    pub exit_code: i32,
    pub outputs: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Start a long-running server process
// ---------------------------------------------------------------------------

/// Spawn a long-running server process. Returns the `Child` handle.
///
/// stdout and stderr are redirected to the provided log file so that
/// the process survives after the CLI exits (no broken-pipe SIGPIPE).
/// The caller is responsible for monitoring and killing.
pub async fn start_server(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_file: &Path,
) -> Result<Child, ProcessError> {
    // Ensure log directory exists.
    if let Some(parent) = log_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .map_err(ProcessError::SpawnFailed)?;
    let stderr_file = file.try_clone().map_err(ProcessError::SpawnFailed)?;

    let child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(stderr_file))
        .kill_on_drop(false) // We manage lifecycle explicitly.
        .spawn()
        .map_err(ProcessError::SpawnFailed)?;

    tracing::info!(
        pid = child.id().unwrap_or(0),
        command = command,
        "started server process"
    );

    Ok(child)
}

// ---------------------------------------------------------------------------
// Run a bash script to completion, capturing VELD_OUTPUT lines
// ---------------------------------------------------------------------------

/// Run a bash command/script to completion. Parses `VELD_OUTPUT key=value`
/// lines from stdout. Returns the collected outputs and exit code.
pub async fn run_bash(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
) -> Result<BashOutput, ProcessError> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .envs(env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ProcessError::SpawnFailed)?;

    let stdout = child.stdout.take().expect("stdout should be piped");

    let mut reader = BufReader::new(stdout).lines();
    let mut outputs = HashMap::new();

    while let Ok(Some(line)) = reader.next_line().await {
        if let Some(kv) = line.strip_prefix("VELD_OUTPUT ") {
            if let Some((key, value)) = kv.split_once('=') {
                outputs.insert(key.trim().to_owned(), value.trim().to_owned());
            }
        }
    }

    let status = child.wait().await.map_err(ProcessError::SpawnFailed)?;

    let exit_code = status.code().unwrap_or(-1);

    if !status.success() {
        tracing::warn!(exit_code, command, "bash step exited with non-zero code");
    }

    Ok(BashOutput { exit_code, outputs })
}

// ---------------------------------------------------------------------------
// Process monitoring
// ---------------------------------------------------------------------------

/// Check whether a process is still alive by sending signal 0.
pub fn is_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    kill(Pid::from_raw(pid as i32), None)
        .map(|_| true)
        .unwrap_or(false)
}

/// Kill a process: send SIGTERM, wait briefly, then SIGKILL if still alive.
pub async fn kill_process(pid: u32) -> Result<(), ProcessError> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let nix_pid = Pid::from_raw(pid as i32);

    // SIGTERM
    if let Err(e) = kill(nix_pid, Signal::SIGTERM) {
        // ESRCH = process already gone — not an error.
        if e != nix::errno::Errno::ESRCH {
            return Err(ProcessError::SignalFailed {
                pid,
                source: std::io::Error::from_raw_os_error(e as i32),
            });
        }
        return Ok(());
    }

    // Wait up to 5 seconds for graceful exit.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if !is_alive(pid) {
            return Ok(());
        }
    }

    // SIGKILL
    tracing::warn!(pid, "process did not exit after SIGTERM, sending SIGKILL");
    if let Err(e) = kill(nix_pid, Signal::SIGKILL) {
        if e != nix::errno::Errno::ESRCH {
            return Err(ProcessError::SignalFailed {
                pid,
                source: std::io::Error::from_raw_os_error(e as i32),
            });
        }
    }

    Ok(())
}
