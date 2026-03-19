use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
// Parsed output from a command step
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub outputs: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Server handle — abstracts over foreground (tokio Child) vs detached (PID only)
// ---------------------------------------------------------------------------

/// Handle to a spawned server process.
///
/// In foreground mode this wraps a `tokio::process::Child` so the orchestrator
/// can manage the async I/O pipes.  In detached mode the process is fully
/// decoupled from the tokio runtime — we only keep the PID.
pub enum ServerHandle {
    /// Foreground: tokio-managed child with piped stdout/stderr.
    Foreground(Child),
    /// Detached: process runs independently; only the PID is tracked.
    Detached { pid: u32 },
}

impl ServerHandle {
    /// Return the OS process ID.
    pub fn pid(&self) -> u32 {
        match self {
            ServerHandle::Foreground(child) => child.id().unwrap_or(0),
            ServerHandle::Detached { pid } => *pid,
        }
    }

    /// Take the inner tokio `Child` if this is a foreground handle.
    pub fn into_child(self) -> Option<Child> {
        match self {
            ServerHandle::Foreground(child) => Some(child),
            ServerHandle::Detached { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Start a long-running server process
// ---------------------------------------------------------------------------

/// Spawn a long-running server process.
///
/// When `foreground` is true, stdout/stderr are piped through background
/// tasks that prepend ISO 8601 timestamps to each line. The process will
/// die when the CLI exits (pipes close).
///
/// When `foreground` is false (detached mode), the process is spawned via
/// `std::process::Command` in its own process group so it is fully
/// independent of the CLI process and the tokio runtime. stdout/stderr are
/// redirected directly to the log file.
pub async fn start_server(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_file: &Path,
    foreground: bool,
) -> Result<ServerHandle, ProcessError> {
    // Ensure log directory exists.
    if let Some(parent) = log_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if foreground {
        start_server_foreground(command, working_dir, env, log_file).await
    } else {
        start_server_detached(command, working_dir, env, log_file)
    }
}

/// Foreground mode: pipe stdout/stderr through timestamping tasks.
async fn start_server_foreground(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_file: &Path,
) -> Result<ServerHandle, ProcessError> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .spawn()
        .map_err(ProcessError::SpawnFailed)?;

    tracing::info!(
        pid = child.id().unwrap_or(0),
        command = command,
        "started server process (foreground)"
    );

    let log_path = log_file.to_path_buf();

    if let Some(stdout) = child.stdout.take() {
        let path = log_path.clone();
        tokio::spawn(async move {
            timestamp_pipe(stdout, &path).await;
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let path = log_path.clone();
        tokio::spawn(async move {
            timestamp_pipe(stderr, &path).await;
        });
    }

    Ok(ServerHandle::Foreground(child))
}

/// Detached mode: spawn via std::process::Command in its own process group.
///
/// Using `std::process::Command` (not tokio) avoids registering the child
/// with tokio's SIGCHLD reaper, and `process_group(0)` ensures the process
/// is in its own process group so it won't receive signals intended for the
/// CLI (e.g. SIGHUP on terminal close, SIGINT from Ctrl-C).
///
/// The process survives after the CLI exits and is reparented to init/launchd.
///
/// stdout/stderr are piped through `veld _timestamp` which prepends ISO 8601
/// timestamps with millisecond precision (pure Rust, no external deps). The
/// entire pipeline (server + timestamper) runs in the same process group and
/// survives CLI exit.
fn start_server_detached(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_file: &Path,
) -> Result<ServerHandle, ProcessError> {
    use std::os::unix::process::CommandExt;

    // Ensure log directory exists and log file is created.
    if let Some(parent) = log_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Wrap the command in a pipeline that timestamps each line via `veld _timestamp`.
    // This is a pure Rust timestamper (no perl/python dependency) with millisecond
    // precision. The entire pipeline runs in its own process group (process_group(0))
    // so it survives CLI exit.
    let log_path_escaped = log_file.to_string_lossy().replace('\'', "'\\''");
    let veld_bin = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("veld"))
        .to_string_lossy()
        .replace('\'', "'\\''");
    let wrapper =
        format!("{{ {command} ; }} 2>&1 | '{veld_bin}' _timestamp --log '{log_path_escaped}'");

    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg(&wrapper)
        .current_dir(working_dir)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0) // own process group — immune to parent signals
        .spawn()
        .map_err(ProcessError::SpawnFailed)?;

    let pid = child.id();

    tracing::info!(
        pid = pid,
        command = command,
        "started server process (detached, pgid=own)"
    );

    // Intentionally drop the std Child handle. The process is fully
    // independent — it will be reparented to init/launchd and reaped
    // by the OS. We only track the PID for later stop/status checks.
    drop(child);

    Ok(ServerHandle::Detached { pid })
}

/// Read lines from an async reader, prepend timestamps, and append to the log file.
async fn timestamp_pipe<R: tokio::io::AsyncRead + Unpin>(reader: R, log_path: &Path) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let timestamp = chrono::Utc::now().to_rfc3339();
                let formatted = format!("[{timestamp}] {line}\n");
                if let Ok(mut file) = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_path)
                    .await
                {
                    let _ = file.write_all(formatted.as_bytes()).await;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Run a command to completion, capturing VELD_OUTPUT lines
// ---------------------------------------------------------------------------

/// Run a command/script to completion. Parses `VELD_OUTPUT key=value`
/// lines from stdout. Returns the collected outputs and exit code.
pub async fn run_command(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
) -> Result<CommandOutput, ProcessError> {
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
        tracing::warn!(exit_code, command, "command step exited with non-zero code");
    }

    Ok(CommandOutput { exit_code, outputs })
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

/// Kill a process and its process group: send SIGTERM, wait briefly, then
/// SIGKILL if still alive. Signals are sent to the process group (negative
/// PID) because detached servers run in their own process group
/// (`process_group(0)`) and the tracked PID is the group leader. This
/// ensures the entire pipeline (server + timestamp wrapper) is cleaned up.
pub async fn kill_process(pid: u32) -> Result<(), ProcessError> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    // Guard against dangerous PIDs:
    // - pid 0: kill(0, sig) sends to our own process group
    // - pid 1: kill(-1, sig) sends to ALL processes we can signal
    // - pid > i32::MAX: wraps negative on cast, producing wrong target
    if pid <= 1 || pid > i32::MAX as u32 {
        return Err(ProcessError::SignalFailed {
            pid,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to signal dangerous pid {pid}"),
            ),
        });
    }

    // Send to the process group (negative PID) to kill all children.
    // Detached servers run in their own process group (process_group(0))
    // and the tracked PID is the group leader.
    let nix_pgid = Pid::from_raw(-(pid as i32));
    let nix_pid = Pid::from_raw(pid as i32);

    // Try killing the process group first.
    let group_kill_result = kill(nix_pgid, Signal::SIGTERM);

    // Fall back to individual PID if group kill fails (ESRCH on the group
    // means the process may not be a group leader).
    if let Err(e) = group_kill_result {
        if e == nix::errno::Errno::ESRCH {
            // Try individual PID — process might already be gone.
            if let Err(e2) = kill(nix_pid, Signal::SIGTERM) {
                if e2 != nix::errno::Errno::ESRCH {
                    return Err(ProcessError::SignalFailed {
                        pid,
                        source: std::io::Error::from_raw_os_error(e2 as i32),
                    });
                }
            }
        } else {
            return Err(ProcessError::SignalFailed {
                pid,
                source: std::io::Error::from_raw_os_error(e as i32),
            });
        }
    }

    // Wait up to 5 seconds for graceful exit.
    // Check both the group leader and the process group itself to ensure
    // the entire pipeline (server + _timestamp) has exited.
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let leader_alive = is_alive(pid);
        let group_alive = kill(nix_pgid, None).is_ok();
        if !leader_alive && !group_alive {
            return Ok(());
        }
    }

    // SIGKILL the group, then fall back to the individual PID.
    tracing::warn!(pid, "process did not exit after SIGTERM, sending SIGKILL");
    if kill(nix_pgid, Signal::SIGKILL).is_err() {
        if let Err(e) = kill(nix_pid, Signal::SIGKILL) {
            if e != nix::errno::Errno::ESRCH {
                return Err(ProcessError::SignalFailed {
                    pid,
                    source: std::io::Error::from_raw_os_error(e as i32),
                });
            }
        }
    }

    Ok(())
}
