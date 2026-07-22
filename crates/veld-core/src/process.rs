use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing;

use crate::db::{Db, LogStream};

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

    #[error("failed to read output file {path}: {source}")]
    OutputFileError {
        path: PathBuf,
        source: std::io::Error,
    },
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

/// Where a server process's output goes: the `server` log stream of one
/// node in the central database.
#[derive(Clone)]
pub struct LogTarget {
    pub db: Db,
    pub project_root: PathBuf,
    pub run_name: String,
    /// Run instance the lines belong to (stringified UUID).
    pub run_id: String,
    pub node: String,
    pub variant: String,
}

impl LogTarget {
    fn append(&self, line: &str) {
        let _ = self.db.append_log(
            &self.project_root,
            &self.run_name,
            Some(&self.run_id),
            Some(&self.node),
            Some(&self.variant),
            LogStream::Server,
            chrono::Utc::now(),
            line,
        );
    }
}

/// Spawn a long-running server process.
///
/// When `foreground` is true, stdout/stderr are piped through background
/// tasks that timestamp each line into the database. The process will
/// die when the CLI exits (pipes close).
///
/// When `foreground` is false (detached mode), the process is spawned via
/// `std::process::Command` in its own process group so it is fully
/// independent of the CLI process and the tokio runtime. stdout/stderr are
/// piped through a detached `veld _log` writer that outlives the CLI.
pub async fn start_server(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_target: LogTarget,
    foreground: bool,
) -> Result<ServerHandle, ProcessError> {
    if foreground {
        start_server_foreground(command, working_dir, env, log_target).await
    } else {
        start_server_detached(command, working_dir, env, &log_target)
    }
}

/// Foreground mode: pipe stdout/stderr through timestamping tasks.
async fn start_server_foreground(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_target: LogTarget,
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

    if let Some(stdout) = child.stdout.take() {
        let target = log_target.clone();
        tokio::spawn(async move {
            log_pipe(stdout, target).await;
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let target = log_target.clone();
        tokio::spawn(async move {
            log_pipe(stderr, target).await;
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
/// stdout/stderr are piped through `veld _log`, which timestamps each line
/// into the central database. The entire pipeline (server + log writer) runs
/// in the same process group and survives CLI exit.
fn start_server_detached(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_target: &LogTarget,
) -> Result<ServerHandle, ProcessError> {
    use std::os::unix::process::CommandExt;

    let sq = |s: &str| s.replace('\'', "'\\''");
    let veld_bin = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("veld"))
        .to_string_lossy()
        .replace('\'', "'\\''");
    let wrapper = format!(
        "{{ {command} ; }} 2>&1 | '{veld_bin}' _log --project-root '{root}' --run '{run}' --run-id '{run_id}' --node '{node}' --variant '{variant}'",
        root = sq(&log_target.project_root.to_string_lossy()),
        run = sq(&log_target.run_name),
        run_id = sq(&log_target.run_id),
        node = sq(&log_target.node),
        variant = sq(&log_target.variant),
    );

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

/// Read lines from an async reader and store them in the database.
async fn log_pipe<R: tokio::io::AsyncRead + Unpin>(reader: R, target: LogTarget) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => target.append(&line),
            Ok(None) => break,
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Run a command to completion, capturing VELD_OUTPUT lines
// ---------------------------------------------------------------------------

/// Run a command/script to completion. Collects outputs from two channels:
///
/// 1. **File-based (preferred):** If `output_file` is `Some`, the file is
///    created before spawning and `VELD_OUTPUT_FILE` is set in the child env.
///    The script writes `key=value` lines to this file. After the process
///    exits the file is read and deleted.
///
/// 2. **Stdout-based (legacy fallback):** `VELD_OUTPUT key=value` lines on
///    stdout are still parsed for backward compatibility but this channel is
///    discouraged because it exposes values in the terminal and logs.
///
/// When both channels produce the same key, the file-based value wins.
pub async fn run_command(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    output_file: Option<&Path>,
) -> Result<CommandOutput, ProcessError> {
    // Prepare the output file and augmented env.
    let mut env = env.clone();
    if let Some(path) = output_file {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ProcessError::OutputFileError {
                path: path.to_path_buf(),
                source: e,
            })?;
        }
        // Create (or truncate) the file with restrictive permissions (0600)
        // since it may contain sensitive values like database passwords.
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .map_err(|e| ProcessError::OutputFileError {
                    path: path.to_path_buf(),
                    source: e,
                })?;
        }
        env.insert(
            "VELD_OUTPUT_FILE".to_owned(),
            path.to_string_lossy().into_owned(),
        );
    }

    let spawn_result = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();

    let mut child = match spawn_result {
        Ok(child) => child,
        Err(e) => {
            // Clean up the output file on spawn failure.
            if let Some(path) = output_file {
                let _ = std::fs::remove_file(path);
            }
            return Err(ProcessError::SpawnFailed(e));
        }
    };

    let stdout = child.stdout.take().expect("stdout should be piped");

    let mut reader = BufReader::new(stdout).lines();
    let mut outputs = HashMap::new();

    // Legacy stdout channel.
    while let Ok(Some(line)) = reader.next_line().await {
        if let Some(kv) = line.strip_prefix("VELD_OUTPUT ") {
            if let Some((key, value)) = kv.split_once('=') {
                outputs.insert(key.trim().to_owned(), value.trim().to_owned());
            }
        }
    }

    let status = child.wait().await.map_err(ProcessError::SpawnFailed)?;

    // Read file-based outputs (overrides stdout for duplicate keys).
    if let Some(path) = output_file {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Some((key, value)) = line.split_once('=') {
                        outputs.insert(key.trim().to_owned(), value.trim().to_owned());
                    } else {
                        tracing::warn!(
                            line,
                            "ignoring malformed line in output file (expected key=value)"
                        );
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %path.display(),
                    "output file was deleted by the script"
                );
            }
            Err(e) => {
                // Clean up before returning error.
                let _ = std::fs::remove_file(path);
                return Err(ProcessError::OutputFileError {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        }
        // Always clean up the temp file.
        let _ = std::fs::remove_file(path);
    }

    let exit_code = status.code().unwrap_or(-1);

    if !status.success() {
        tracing::warn!(exit_code, command, "command step exited with non-zero code");
    }

    Ok(CommandOutput { exit_code, outputs })
}

// ---------------------------------------------------------------------------
// Run a command to completion while streaming its output live
// ---------------------------------------------------------------------------

/// Run a command/script to completion, streaming its output live.
///
/// Unlike [`run_command`], every stdout line is echoed to the process's own
/// stdout and every stderr line to its stderr, so a human (or CI, or a coding
/// agent) sees the output as it happens — this is what makes a `--oneshot`
/// terminal node (e.g. an end-to-end test runner) print its results inline.
/// When `log_target` is `Some`, each line is also timestamped into the
/// database `server` stream so `veld logs --node <n>` works after the run.
///
/// `VELD_OUTPUT key=value` control lines on stdout are still parsed (for
/// declared outputs) but are never echoed or logged — they are machinery, not
/// program output.
///
/// The child runs in its own process group so a Ctrl+C delivered to the CLI's
/// controlling terminal is not auto-forwarded to it; instead we catch the
/// signal, kill the whole group, and report exit code `130` (SIGINT). This
/// keeps interruption deterministic regardless of how the child handles
/// signals itself.
pub async fn run_command_streaming(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
    output_file: Option<&Path>,
    log_target: Option<LogTarget>,
) -> Result<CommandOutput, ProcessError> {
    use tokio::io::AsyncWriteExt;

    // Prepare the output file and augmented env (mirrors `run_command`).
    let mut env = env.clone();
    if let Some(path) = output_file {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ProcessError::OutputFileError {
                path: path.to_path_buf(),
                source: e,
            })?;
        }
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .map_err(|e| ProcessError::OutputFileError {
                    path: path.to_path_buf(),
                    source: e,
                })?;
        }
        env.insert(
            "VELD_OUTPUT_FILE".to_owned(),
            path.to_string_lossy().into_owned(),
        );
    }

    let spawn_result = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(working_dir)
        .envs(&env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0) // own group — we forward Ctrl+C by killing the group
        .spawn();

    let mut child = match spawn_result {
        Ok(child) => child,
        Err(e) => {
            if let Some(path) = output_file {
                let _ = std::fs::remove_file(path);
            }
            return Err(ProcessError::SpawnFailed(e));
        }
    };

    let pid = child.id().unwrap_or(0);
    let stdout = child.stdout.take().expect("stdout should be piped");
    let stderr = child.stderr.take().expect("stderr should be piped");

    // Each stream is drained to completion by its own task, so a Ctrl+C
    // (handled in the select below) can never cancel a partial read — the
    // tasks own their readers and stop only at EOF (or an unrecoverable read
    // error). Lines are decoded
    // lossily: a test runner may emit non-UTF-8 bytes, and a bad byte must
    // replace one character, not truncate the rest of the stream (which
    // `Lines`/`str`-based reads would do on the first `InvalidData`).
    //
    // stdout carries the program's real output; `VELD_OUTPUT key=value`
    // control lines are peeled off it and sent back over `out_tx` instead of
    // being echoed or logged. stderr is forwarded verbatim.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    let out_log = log_target.clone();
    let out_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut w = tokio::io::stdout();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while matches!(buf.last(), Some(b'\n' | b'\r')) {
                        buf.pop();
                    }
                    let line = String::from_utf8_lossy(&buf);
                    if let Some(kv) = line.strip_prefix("VELD_OUTPUT ") {
                        if let Some((key, value)) = kv.split_once('=') {
                            let _ = out_tx.send((key.trim().to_owned(), value.trim().to_owned()));
                        }
                        // Control line — never echoed or logged.
                    } else {
                        let _ = w.write_all(line.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                        let _ = w.flush().await;
                        if let Some(ref t) = out_log {
                            t.append(&line);
                        }
                    }
                }
            }
        }
    });
    let err_log = log_target.clone();
    let err_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut w = tokio::io::stderr();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while matches!(buf.last(), Some(b'\n' | b'\r')) {
                        buf.pop();
                    }
                    let line = String::from_utf8_lossy(&buf);
                    let _ = w.write_all(line.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                    let _ = w.flush().await;
                    if let Some(ref t) = err_log {
                        t.append(&line);
                    }
                }
            }
        }
    });

    // Wait for both drain tasks to finish; a Ctrl+C kills the child's process
    // group (the tasks then hit EOF) and reports the conventional 130 code.
    let mut interrupted = false;
    let drain = async {
        let _ = tokio::join!(out_task, err_task);
    };
    tokio::pin!(drain);
    tokio::select! {
        _ = &mut drain => {}
        _ = tokio::signal::ctrl_c() => {
            interrupted = true;
            if pid > 1 {
                let _ = kill_process(pid).await;
            }
            // Bounded wait for the drain tasks to finish after the kill: they
            // normally hit EOF immediately once the pipes close, but don't hang
            // the interrupt forever if our own stdout consumer has stalled.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(10), &mut drain).await;
        }
    }

    let mut outputs = HashMap::new();
    while let Ok((key, value)) = out_rx.try_recv() {
        outputs.insert(key, value);
    }

    let status = child.wait().await.map_err(ProcessError::SpawnFailed)?;

    // Read file-based outputs (overrides stdout for duplicate keys).
    if let Some(path) = output_file {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Some((key, value)) = line.split_once('=') {
                        outputs.insert(key.trim().to_owned(), value.trim().to_owned());
                    } else {
                        tracing::warn!(
                            line,
                            "ignoring malformed line in output file (expected key=value)"
                        );
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(path = %path.display(), "output file was deleted by the script");
            }
            Err(e) => {
                let _ = std::fs::remove_file(path);
                return Err(ProcessError::OutputFileError {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        }
        let _ = std::fs::remove_file(path);
    }

    // On Ctrl+C, report the conventional SIGINT exit code regardless of how
    // the child actually terminated.
    let exit_code = if interrupted {
        130
    } else {
        status.code().unwrap_or(-1)
    };

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

#[cfg(test)]
mod streaming_tests {
    use super::*;

    /// A non-zero exit is the terminal node's *result*: `run_command_streaming`
    /// must surface it as `exit_code` (never an error) so `--oneshot` can
    /// propagate it, and must still parse `VELD_OUTPUT` control lines.
    #[tokio::test]
    async fn captures_nonzero_exit_and_outputs() {
        let env = HashMap::new();
        let dir = std::env::temp_dir();
        let out = run_command_streaming(
            "echo hello; echo 'VELD_OUTPUT foo=bar'; echo oops 1>&2; exit 3",
            &dir,
            &env,
            None,
            None,
        )
        .await
        .expect("streaming run should not error on non-zero exit");
        assert_eq!(out.exit_code, 3);
        assert_eq!(out.outputs.get("foo").map(String::as_str), Some("bar"));
    }

    #[tokio::test]
    async fn zero_exit_no_outputs() {
        let env = HashMap::new();
        let dir = std::env::temp_dir();
        let out = run_command_streaming("true", &dir, &env, None, None)
            .await
            .expect("streaming run should succeed");
        assert_eq!(out.exit_code, 0);
        assert!(out.outputs.is_empty());
    }

    /// A raw non-UTF-8 byte on stdout must not truncate the stream: the later
    /// `VELD_OUTPUT` line is still parsed (lossy decode replaces the bad byte).
    #[tokio::test]
    async fn lossy_decode_survives_non_utf8() {
        let env = HashMap::new();
        let dir = std::env::temp_dir();
        let out = run_command_streaming(
            "printf '\\377\\n'; echo 'VELD_OUTPUT foo=bar'",
            &dir,
            &env,
            None,
            None,
        )
        .await
        .expect("streaming run should tolerate non-UTF-8 output");
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.outputs.get("foo").map(String::as_str), Some("bar"));
    }
}
