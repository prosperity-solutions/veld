use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tracing;

use crate::config::CommandSpec;
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

/// Where a command step's live output goes: one call per batch of lines, from
/// the reader task that drains the pipe.
///
/// A `command` step's output used to be thrown away — stdout was read only for
/// `VELD_OUTPUT` control lines and stderr was inherited straight onto the
/// terminal, so a `docker build` or `pnpm install` node scrolled past the
/// progress bars and was in no log the user could read afterwards. The sink is
/// how the caller re-attaches that output to something: the database, the
/// progress channel, or both. `None` keeps a step's output off the record
/// entirely (`skip_if` probes, which are predicates).
///
/// It takes a **batch**, not a line, because the caller's per-call cost is not
/// free: the progress renderer redraws every spinner per `println`, and a build
/// tool emits tens of thousands of lines. One call per read from the pipe keeps
/// that proportional to I/O rather than to line count.
pub type LineSink = std::sync::Arc<dyn Fn(&[String]) + Send + Sync>;

/// Longest line kept whole before it is split and sinked as-is.
///
/// A progress meter that only ever emits `\r` (curl, some installers) has no
/// line breaks at all from a `\n`-only reader's point of view — without a cap
/// its output accumulates in memory for the life of the step and lands as one
/// enormous row. 64 KiB is far past any real log line.
///
/// `pub` because the detached `veld _log` pump in `crates/veld/src/main.rs`
/// needs the same ceiling: it buffers lines for its writer thread, so an
/// uncapped line there is an uncapped queue.
///
/// **Do not read that as "the readers are capped".** There are five loops in
/// veld that assemble a line from a pipe, and only two honour this:
///
/// | reader | caps? |
/// |---|---|
/// | [`drain_pipe`] (`command` steps via `LineSink`) | yes |
/// | `veld _log` (detached servers) | yes, at the enqueue |
/// | [`log_pipe`] (foreground servers) | **no** |
/// | `run_command_streaming` stdout | **no** |
/// | `run_command_streaming` stderr | **no** |
///
/// The three that do not cannot be fixed by adding a length check, which is why
/// they are still like this: `Lines::next_line` and `read_until` do not return
/// until they see a delimiter or EOF, so there is no point at which a caller can
/// intervene. `drain_pipe` gets this right by reading with `fill_buf` and
/// assembling lines itself, and unifying the other three onto it is a refactor of
/// its own rather than a length check. Until then a `\r`-only progress meter
/// still accumulates without bound on those paths — a pre-existing hazard this
/// table exists to stop the next reader from assuming away.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// How long the pipe readers get to finish after the step's own process exits.
///
/// A step that backgrounds something (`sh -c 'server >/dev/null &'`) leaves a
/// descendant holding the write end of these pipes, so EOF may never come.
/// Waiting for it wedges the CLI for as long as that descendant lives — and
/// before this file captured stderr, not wedging is exactly what the daemon
/// relied on (see `veld-daemon/src/management.rs`, spawn-log capture). Whatever
/// is still buffered when the child exits drains in microseconds; this bounds
/// only the pathological case.
const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// The pipe readers, aborted when they go out of scope.
///
/// They must not outlive the call that spawned them. A [`LineSink`] captures
/// whatever the caller wired it to — the orchestrator's progress channel, among
/// other things — so a reader that survives its `run_command` keeps a sender
/// alive, and the CLI's progress task, which ends when the last sender drops,
/// never finishes. Ctrl+C during a `command` node drops this future without
/// reaching the grace period above, and that is exactly when it matters: the
/// step's process is still running, so the pipes are still open, and `veld
/// start` would hang instead of tearing the run down.
struct DrainTasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for DrainTasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// Assembles lines from a byte stream, with [`MAX_LINE_BYTES`] enforced **while**
/// accumulating.
///
/// **This exists because a length check cannot be bolted onto the obvious
/// readers.** `Lines::next_line` and `read_until` do not return until they see a
/// delimiter or EOF, so a stream that never emits one — a `\r`-only progress
/// meter, the exact case the cap is for — grows their buffer without bound before
/// any caller-side check can look at it. Three of veld's pipe readers were built
/// on those two functions and were therefore uncapped: [`log_pipe`] (foreground
/// servers) and both drain tasks of [`run_command_streaming`]. They now share this.
///
/// **Line semantics, matching what those readers already did**, so adopting it
/// changes only the bound:
///
/// - `\n` ends a line; a trailing `\r` is stripped from it (`\r\n` therefore
///   stores neither).
/// - An *embedded* `\r` is data and is kept. It is not a line terminator here —
///   deliberately unlike [`drain_pipe`], whose `LineSink` contract splits on it.
///   The renderers want it kept: `ui/src/shared/ansi.ts` honours carriage return
///   inside a row so a progress line collapses to its last frame, and splitting
///   instead makes the log panel render every frame.
/// - At the cap the line is **split, not truncated**, on a character boundary. No
///   bytes are lost and no character is corrupted across the break.
///
/// One deliberate unification: a line ending `\r\r\n` stores `x` rather than
/// `x\r`. `read_until`'s pop-loop already did that; tokio's `Lines` stripped only
/// one `\r`. Nothing can observe the difference — a *trailing* CR is dropped by
/// the renderer anyway.
#[derive(Default)]
pub struct LineAssembler {
    pending: Vec<u8>,
}

impl LineAssembler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes, appending every line it completes to `out`.
    pub fn push_chunk(&mut self, chunk: &[u8], out: &mut Vec<String>) {
        for &byte in chunk {
            if byte == b'\n' {
                while matches!(self.pending.last(), Some(b'\r')) {
                    self.pending.pop();
                }
                out.push(self.take_pending());
                continue;
            }
            self.pending.push(byte);
            if self.pending.len() >= MAX_LINE_BYTES {
                // Cut on a character boundary and keep the trailing partial
                // character for the next line, so one character is not corrupted
                // on both sides of a break the process never asked for. No
                // terminator is stripped here: this is an arbitrary split point,
                // not the end of a line.
                let keep = self.pending.len() - trailing_partial_char(&self.pending);
                let tail = self.pending.split_off(keep);
                out.push(self.take_pending());
                self.pending = tail;
            }
        }
    }

    /// The bytes left over at EOF: output that never got its newline, which is
    /// still output. `None` when there is nothing, or nothing but terminators.
    pub fn finish(&mut self) -> Option<String> {
        while matches!(self.pending.last(), Some(b'\r' | b'\n')) {
            self.pending.pop();
        }
        if self.pending.is_empty() {
            None
        } else {
            Some(self.take_pending())
        }
    }

    fn take_pending(&mut self) -> String {
        let line = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        line
    }
}

/// How many bytes at the end of `buf` are the start of a multi-byte character
/// whose remaining bytes have not arrived (0 if it ends on a boundary).
///
/// A UTF-8 character is at most 4 bytes, so at most 3 can be pending.
/// `pub` so the `veld _log` pump can split an over-long line the same way
/// [`drain_pipe`] does — same ceiling *and* same behaviour at it, rather than one
/// path splitting and the other truncating.
pub fn trailing_partial_char(buf: &[u8]) -> usize {
    for back in 1..=3.min(buf.len()) {
        let byte = buf[buf.len() - back];
        // Continuation bytes are 10xxxxxx; anything else starts a character.
        if byte & 0b1100_0000 != 0b1000_0000 {
            let width = match byte {
                0x00..=0x7F => 1,
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                // Not a lead byte at all — invalid input, nothing to hold back.
                _ => return 0,
            };
            return if width > back { back } else { 0 };
        }
    }
    0
}

/// Drain one pipe, sinking whole lines in batches.
///
/// Splits on `\n` **and** `\r`, because the tools this exists for (build tools,
/// installers, `curl`) redraw a progress line with a bare carriage return; a
/// `\r\n` counts once, and a `\r` that follows a line break terminates nothing.
/// Lines are decoded lossily: a stray non-UTF-8 byte must cost one character,
/// not the rest of the stream.
///
/// `outputs` is `Some` only for stdout, the legacy `VELD_OUTPUT key=value`
/// channel. Control lines are peeled off either stream and never sinked.
async fn drain_pipe<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    sink: Option<LineSink>,
    outputs: Option<tokio::sync::mpsc::UnboundedSender<(String, String)>>,
) {
    let mut reader = BufReader::new(reader);
    let mut pending: Vec<u8> = Vec::new();
    // Both carried across reads: a `\r\n` can straddle two chunks.
    let mut last_was_cr = false;
    // Whether that `\r` ended a line with content in it. It is what separates a
    // meter redraw from a real blank line: in `100%\r\n` the `\r` terminated
    // `100%` and the `\n` completes the same terminator, but in `\r\n\r\n` the
    // second `\r` terminated nothing, so its `\n` completes a blank line.
    let mut cr_ended_content = false;

    let take_line = |pending: &mut Vec<u8>, batch: &mut Vec<String>| {
        let line = String::from_utf8_lossy(pending).into_owned();
        pending.clear();
        match line.strip_prefix("VELD_OUTPUT ") {
            Some(kv) => {
                if let (Some(tx), Some((key, value))) = (outputs.as_ref(), kv.split_once('=')) {
                    let _ = tx.send((key.trim().to_owned(), value.trim().to_owned()));
                }
                // Control line — machinery, never sinked.
            }
            None => batch.push(line),
        }
    };

    loop {
        let chunk = match reader.fill_buf().await {
            Ok([]) => break,
            // A signal can interrupt the read with nothing wrong with the pipe.
            // Treating that as EOF would stop draining while the child is still
            // writing, and it would then block forever on a full pipe.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
            Ok(buf) => buf.to_vec(),
        };
        reader.consume(chunk.len());

        let mut batch: Vec<String> = Vec::new();
        for byte in chunk {
            match byte {
                b'\r' => {
                    // A `\r` with nothing before it ends no line: it is a meter
                    // redrawing (`50%\r\r100%`) or following a break (`\n\r`).
                    // Whether it nonetheless starts a blank line is decided by
                    // the next byte — only `\r\n` there means the step really
                    // printed one.
                    cr_ended_content = !pending.is_empty();
                    if cr_ended_content {
                        take_line(&mut pending, &mut batch);
                    }
                    last_was_cr = true;
                }
                b'\n' => {
                    // Emit unless this `\n` is the tail of a `\r\n` whose `\r`
                    // already ended the line.
                    if !(last_was_cr && cr_ended_content) {
                        take_line(&mut pending, &mut batch);
                    }
                    last_was_cr = false;
                    cr_ended_content = false;
                }
                b => {
                    pending.push(b);
                    last_was_cr = false;
                    if pending.len() >= MAX_LINE_BYTES {
                        // Cut on a character boundary: splitting a multi-byte
                        // char would corrupt one character on each side of a
                        // break the step never asked for.
                        let keep = pending.len() - trailing_partial_char(&pending);
                        let tail = pending.split_off(keep);
                        take_line(&mut pending, &mut batch);
                        pending = tail;
                    }
                }
            }
        }
        if !batch.is_empty() {
            if let Some(ref s) = sink {
                s(&batch);
            }
        }
    }

    // Output that never got its newline (a truncated final line) is still
    // output — dropping it would hide the last thing a step said.
    if !pending.is_empty() {
        let mut batch = Vec::new();
        take_line(&mut pending, &mut batch);
        if !batch.is_empty() {
            if let Some(ref s) = sink {
                s(&batch);
            }
        }
    }
}

impl LogTarget {
    /// Write a batch of individually-stamped lines as one transaction.
    ///
    /// There is deliberately no single-line sibling: every reader on this type
    /// is a pipe drain, and one autocommit `INSERT` per line is the 6.47-WAL-
    /// -pages-per-line shape that [`Db::append_logs`] documents. Feed it from a
    /// [`crate::logging::LogBatch`].
    ///
    /// A failed write does not stop the drain — this runs on the task reading a
    /// child's stdout, and a drain that stops because the database was busy
    /// blocks the child on a full pipe. But it is **reported**, and that is a
    /// change rather than the previous behaviour continued: one autocommit
    /// `INSERT` per line lost exactly one line when it failed, and one
    /// transaction per batch loses up to `LOG_BATCH_MAX_LINES` of them. Silently
    /// dropping 512 lines on the path that carries a foreground server's output is
    /// the shape of gap #332 is a report about.
    ///
    /// **Reported to `tracing`, not to the database — unlike `veld _log`'s
    /// otherwise-identical notice.** A second write here would sit on the same
    /// connection with the same 10-second `busy_timeout`, so a busy database would
    /// stall this drain for up to 20 seconds per batch instead of 10 — doubling
    /// the exact harm the paragraph above gives as the reason for swallowing the
    /// error. `veld _log` can afford the database write because its writer thread
    /// is off the drain path by construction; this task is the drain. Both readers
    /// that reach here run inside the foreground `veld` process, so a `warn!`
    /// lands where somebody is looking.
    fn append_batch(&self, lines: &[(chrono::DateTime<chrono::Utc>, String)]) {
        if lines.is_empty() {
            return;
        }
        if self
            .db
            .append_logs(
                &self.project_root,
                &self.run_name,
                Some(&self.run_id),
                Some(&self.node),
                Some(&self.variant),
                LogStream::Server,
                lines,
            )
            .is_err()
        {
            tracing::warn!(
                node = self.node.as_str(),
                variant = self.variant.as_str(),
                lines = lines.len(),
                "dropped log line(s): the batch could not be written to the database"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Command builders — the single owner of "how a CommandSpec becomes a process"
// ---------------------------------------------------------------------------

/// Build a `tokio::process::Command` for a spec: `argv` is spawned directly,
/// `shell` goes through `sh -c`.
///
/// Every async spawn site in the tree routes through this (and [`std_command`]
/// for the sync ones), so `argv` cannot mean "no shell" in one place and
/// "re-parsed by a shell" in another. An empty `argv` would panic on
/// `argv[0]`, so it is rejected here rather than at the OS boundary.
pub fn tokio_command(spec: &CommandSpec) -> Result<Command, ProcessError> {
    match spec {
        CommandSpec::Argv(argv) => {
            let (program, rest) = argv.split_first().ok_or_else(empty_argv)?;
            let mut cmd = Command::new(program);
            cmd.args(rest);
            Ok(cmd)
        }
        CommandSpec::Shell(s) => {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(s);
            Ok(cmd)
        }
    }
}

/// The `std::process::Command` twin of [`tokio_command`], for the sync spawn
/// sites (detached servers, `veld action`).
pub fn std_command(spec: &CommandSpec) -> Result<std::process::Command, ProcessError> {
    match spec {
        CommandSpec::Argv(argv) => {
            let (program, rest) = argv.split_first().ok_or_else(empty_argv)?;
            let mut cmd = std::process::Command::new(program);
            cmd.args(rest);
            Ok(cmd)
        }
        CommandSpec::Shell(s) => {
            let mut cmd = std::process::Command::new("sh");
            cmd.arg("-c").arg(s);
            Ok(cmd)
        }
    }
}

fn empty_argv() -> ProcessError {
    ProcessError::SpawnFailed(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "argv is empty — there is no program to run",
    ))
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
    command: &CommandSpec,
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
    command: &CommandSpec,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_target: LogTarget,
) -> Result<ServerHandle, ProcessError> {
    let mut child = tokio_command(command)?
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
        command = command.display(),
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

/// Single-quote a string for safe inclusion in a `sh -c` script.
fn sq(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// The argv of the log-sink stage of the detached pipeline: veld's own binary
/// re-invoked as `veld _log`, which timestamps each line into the database.
fn log_sink_argv(log_target: &LogTarget) -> Vec<String> {
    let veld_bin = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("veld"))
        .to_string_lossy()
        .into_owned();
    vec![
        veld_bin,
        "_log".to_owned(),
        "--project-root".to_owned(),
        log_target.project_root.to_string_lossy().into_owned(),
        "--run".to_owned(),
        log_target.run_name.clone(),
        "--run-id".to_owned(),
        log_target.run_id.clone(),
        "--node".to_owned(),
        log_target.node.clone(),
        "--variant".to_owned(),
        log_target.variant.clone(),
    ]
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
    command: &CommandSpec,
    working_dir: &Path,
    env: &HashMap<String, String>,
    log_target: &LogTarget,
) -> Result<ServerHandle, ProcessError> {
    spawn_detached(command, working_dir, env, &log_sink_argv(log_target))
}

/// Spawn the detached pipeline with an explicit log-sink argv.
///
/// Split out from [`start_server_detached`] so the RC1 characterization tests
/// can substitute a stub sink: the real sink is `veld _log`, and a test process
/// has no veld binary at `current_exe()`. Production always supplies
/// [`log_sink_argv`]; the pipeline shape, tracked PID, and process-group
/// leadership are identical either way, which is exactly what those tests pin.
#[doc(hidden)]
pub fn spawn_detached(
    command: &CommandSpec,
    working_dir: &Path,
    env: &HashMap<String, String>,
    sink_argv: &[String],
) -> Result<ServerHandle, ProcessError> {
    use std::os::unix::process::CommandExt;

    // Only veld's own `_log` arguments are shell-quoted; they are ours, not the
    // author's.
    let sink = sink_argv
        .iter()
        .map(|a| format!("'{}'", sq(a)))
        .collect::<Vec<_>>()
        .join(" ");

    // The pipeline stays a shell pipeline. For `argv`, the node command is passed
    // as **positional parameters** and expanded with `"$@"`, which produces
    // exactly one word per element regardless of spaces, globs, quotes, or
    // newlines — so an interpolated value can never change the argument count,
    // even here.
    //
    // Deliberately NOT re-joining argv into the script, and deliberately NOT
    // rebuilding the pipeline with manual file descriptors: the process topology,
    // tracked PID, process-group leadership, and the `stats.rs` parent-tree walk
    // must stay byte-identical to the shell-string case. Real fds would change
    // what `is_alive` tracks (breaking `cleanup_dead_runs`, `veld status`, the
    // monitor, and GC for servers that double-fork) and would change the
    // `node_stats` sampled *set*, producing an unexplained step change in the UI
    // graph. See the RC1 characterization tests below.
    let (wrapper, positional): (String, &[String]) = match command {
        CommandSpec::Shell(s) => (format!("{{ {s} ; }} 2>&1 | {sink}"), &[]),
        CommandSpec::Argv(argv) => {
            if argv.is_empty() {
                return Err(empty_argv());
            }
            (format!("{{ \"$@\" ; }} 2>&1 | {sink}"), argv.as_slice())
        }
    };

    let mut builder = std::process::Command::new("sh");
    builder.arg("-c").arg(&wrapper);
    if !positional.is_empty() {
        // `$0` for the shell itself, then one argument per argv element.
        builder.arg("sh").args(positional);
    }
    let child = builder
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
        command = command.display(),
        "started server process (detached, pgid=own)"
    );

    // Intentionally drop the std Child handle. The process is fully
    // independent — it will be reparented to init/launchd and reaped
    // by the OS. We only track the PID for later stop/status checks.
    drop(child);

    Ok(ServerHandle::Detached { pid })
}

/// Write a batch to an optional log target, clearing it either way.
///
/// The `None` case still has to empty the batch: a step with no log target never
/// pushes, so this is belt-and-braces rather than a live path, and leaving lines
/// behind would arm a deadline that never clears.
fn flush_out(target: &Option<LogTarget>, batch: &mut crate::logging::LogBatch) {
    let lines = batch.take();
    if let Some(t) = target {
        t.append_batch(&lines);
    }
}

/// Read lines from an async reader and store them in the database, in batches.
///
/// A foreground server's stdout is one of the highest-volume writers veld has,
/// and it used to be one autocommit `INSERT` per line. The batch flushes on
/// `LOG_BATCH_MAX_LINES` under load and on `LOG_BATCH_FLUSH` when the server goes
/// quiet, so a line is never held longer than the flush window — well inside the
/// 200 ms the interactive `--follow` loops poll at.
async fn log_pipe<R: tokio::io::AsyncRead + Unpin>(reader: R, target: LogTarget) {
    use crate::logging::LogBatch;

    // `fill_buf` + `LineAssembler`, not `BufReader::lines()`. `Lines::next_line`
    // does not return until it sees a `\n`, so a server emitting a `\r`-only
    // progress meter grew its buffer without bound — `MAX_LINE_BYTES` could not
    // be applied from out here because there was no point at which to apply it.
    // The assembler enforces the cap while accumulating and keeps this path's line
    // semantics unchanged otherwise.
    let mut reader = BufReader::new(reader);
    let mut assembler = LineAssembler::new();
    let mut batch = LogBatch::new();
    let mut lines: Vec<String> = Vec::new();

    loop {
        // With nothing buffered there is no deadline to honour — block for the
        // first read rather than spinning on an expired timeout.
        let chunk = match batch.deadline() {
            None => reader.fill_buf().await.map(<[u8]>::to_vec),
            Some(deadline) => {
                match tokio::time::timeout_at(deadline.into(), reader.fill_buf()).await {
                    Ok(read) => read.map(<[u8]>::to_vec),
                    Err(_) => {
                        // Deadline reached with a partial batch. `fill_buf` is
                        // cancel-safe — a cancelled call consumes nothing, so the
                        // bytes are still there for the next one.
                        target.append_batch(&batch.take());
                        continue;
                    }
                }
            }
        };

        match chunk {
            Ok(chunk) if chunk.is_empty() => break, // EOF
            Ok(chunk) => {
                reader.consume(chunk.len());
                lines.clear();
                assembler.push_chunk(&chunk, &mut lines);
                for line in lines.drain(..) {
                    batch.push(line);
                    if batch.is_full() {
                        target.append_batch(&batch.take());
                    }
                }
            }
            // A signal can interrupt the read with nothing wrong with the pipe;
            // treating that as EOF would stop draining while the server is still
            // writing, and it would then block forever on a full pipe.
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }

    // EOF: the last thing a dying server said is the line most worth having, and
    // it may never have got its newline.
    if let Some(last) = assembler.finish() {
        batch.push(last);
    }
    target.append_batch(&batch.take());
}

/// Drain one of a `command` step's pipes: echo every line to the terminal as it
/// arrives, and batch the same lines into the database.
///
/// **One function for both pipes.** They were two near-identical `read_until`
/// loops, and every defect found in one had to be found again in the other —
/// which is how a trailing-line loss and a spurious blank row each shipped twice.
///
/// `fill_buf` + [`LineAssembler`] rather than `read_until`, which buys two things.
/// `MAX_LINE_BYTES` is now enforced while accumulating, so a step emitting a
/// `\r`-only progress meter no longer grows an unbounded buffer — `read_until`
/// gave no point at which a caller could apply the cap. And `fill_buf` is
/// cancel-safe in the way that matters: a cancelled call consumes nothing, where a
/// cancelled `read_until` left partial bytes in the caller's buffer and reported
/// only its own byte count, which is what made the flush deadline able to lose the
/// last line of a step.
///
/// `outputs` is `Some` for stdout only: `VELD_OUTPUT key=value` control lines are
/// peeled off there and sent back rather than echoed or logged, because they can
/// carry sensitive node outputs. stderr is forwarded verbatim.
async fn drain_to_terminal_and_log<R, W>(
    reader: R,
    mut terminal: W,
    log: Option<LogTarget>,
    outputs: Option<tokio::sync::mpsc::UnboundedSender<(String, String)>>,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut assembler = LineAssembler::new();
    // The terminal echo stays line-at-a-time and immediate — only the *database*
    // write is batched, so nothing about what a person watching the run sees
    // changes. With no log target the batch is never pushed, so `deadline()` stays
    // `None` and the read is never wrapped.
    let mut batch = crate::logging::LogBatch::new();
    let mut lines: Vec<String> = Vec::new();

    loop {
        let chunk = match batch.deadline() {
            None => reader.fill_buf().await.map(<[u8]>::to_vec),
            Some(deadline) => {
                match tokio::time::timeout_at(deadline.into(), reader.fill_buf()).await {
                    Ok(read) => read.map(<[u8]>::to_vec),
                    Err(_) => {
                        flush_out(&log, &mut batch);
                        continue;
                    }
                }
            }
        };

        let chunk = match chunk {
            Ok(chunk) if chunk.is_empty() => break, // EOF
            Ok(chunk) => chunk,
            // A signal can interrupt the read with nothing wrong with the pipe.
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        reader.consume(chunk.len());

        lines.clear();
        assembler.push_chunk(&chunk, &mut lines);
        for line in lines.drain(..) {
            emit_step_line(line, &mut terminal, &log, &outputs, &mut batch).await;
        }
    }

    // Output that never got its newline is still output — the last thing a failing
    // step printed is the line most worth having.
    if let Some(last) = assembler.finish() {
        emit_step_line(last, &mut terminal, &log, &outputs, &mut batch).await;
    }
    flush_out(&log, &mut batch);
}

/// Echo one line and queue it for the database, peeling off a control line.
async fn emit_step_line<W: tokio::io::AsyncWrite + Unpin>(
    line: String,
    terminal: &mut W,
    log: &Option<LogTarget>,
    outputs: &Option<tokio::sync::mpsc::UnboundedSender<(String, String)>>,
    batch: &mut crate::logging::LogBatch,
) {
    use tokio::io::AsyncWriteExt;

    if let Some(tx) = outputs {
        if let Some(kv) = line.strip_prefix("VELD_OUTPUT ") {
            if let Some((key, value)) = kv.split_once('=') {
                let _ = tx.send((key.trim().to_owned(), value.trim().to_owned()));
            }
            // Control line — never echoed or logged.
            return;
        }
    }

    let _ = terminal.write_all(line.as_bytes()).await;
    let _ = terminal.write_all(b"\n").await;
    let _ = terminal.flush().await;

    if log.is_some() {
        batch.push(line);
        if batch.is_full() {
            flush_out(log, batch);
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
///
/// Both pipes are captured, never inherited: everything the step prints is
/// handed to `sink` as it arrives, in batches (see [`LineSink`]). Inheriting
/// stderr wrote a `docker build` straight over the progress bars and left
/// nothing behind for `veld logs`.
///
/// `VELD_OUTPUT ` control lines are peeled off **both** streams and never
/// sinked — they can carry secrets, which is the whole reason the file channel
/// exists. Only the stdout ones become outputs; stderr was never an output
/// channel, so a control line there is dropped rather than parsed.
///
/// The peel is a literal prefix match and nothing more. It does not, and cannot,
/// find a value a step prints some other way — `set -x` traces the line as
/// `+ echo 'VELD_OUTPUT token=…'`, which is not a control line and is recorded
/// like any other output. Steps that handle secrets should use
/// `$VELD_OUTPUT_FILE`, which never touches a stream at all.
///
/// stdin is `/dev/null`: with both outputs captured, a step that prompts would
/// otherwise block on a prompt nobody can see. Failing fast on EOF is the
/// legible outcome, and it matches every other spawn path in veld.
///
/// **Deliberately different from [`run_command_streaming`]**, which reads like a
/// near-twin: that one echoes to the CLI's own stdout/stderr (a `--oneshot`
/// terminal node's output *is* the command's result), splits on `\n` only, and
/// drains before waiting because it owns the Ctrl+C handling for its child.
/// This one routes output through a sink instead, splits on `\r` too, and waits
/// on the child *before* draining — a `command` step is not the run's output,
/// and it may leave a descendant holding these pipes open. Don't unify them
/// without deciding which of those each caller needs.
pub async fn run_command(
    command: &CommandSpec,
    working_dir: &Path,
    env: &HashMap<String, String>,
    output_file: Option<&Path>,
    sink: Option<LineSink>,
) -> Result<CommandOutput, ProcessError> {
    run_command_observed(command, working_dir, env, output_file, sink, None).await
}

/// [`run_command`], plus a callback handed the spawned process's PID.
///
/// `on_spawn` exists for one caller: the resource sampler needs a root to walk
/// while a build or an install runs, and a step's PID is otherwise created and
/// destroyed entirely inside this function. It fires once, immediately after a
/// successful spawn and before any output is drained, so a step that lives two
/// seconds is still observable.
///
/// **What the callback receives is a loan, not a handle.** The PID is valid only
/// until this function returns; after that the process has been reaped and the
/// number is eligible for reuse. Record measurements against it — never store
/// it, never signal it, and never persist it as node state (see
/// [`crate::stats::StepObserver`] for why that distinction is load-bearing).
pub async fn run_command_observed(
    command: &CommandSpec,
    working_dir: &Path,
    env: &HashMap<String, String>,
    output_file: Option<&Path>,
    sink: Option<LineSink>,
    on_spawn: Option<Box<dyn FnOnce(u32) + Send>>,
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

    let spawn_result = match tokio_command(command) {
        Ok(mut cmd) => cmd
            .current_dir(working_dir)
            .envs(&env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn(),
        Err(e) => {
            if let Some(path) = output_file {
                let _ = std::fs::remove_file(path);
            }
            return Err(e);
        }
    };

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

    // Before draining anything: a short step must be observable, and the
    // observer only has until `child.wait()` returns to look at this PID.
    // `id()` is `None` only once the child has been reaped, which cannot have
    // happened yet.
    if let (Some(on_spawn), Some(pid)) = (on_spawn, child.id()) {
        on_spawn(pid);
    }

    let stdout = child.stdout.take().expect("stdout should be piped");
    let stderr = child.stderr.take().expect("stderr should be piped");

    // Both pipes are drained by their own task. Reading them in sequence would
    // deadlock the moment a step writes more than a pipe buffer to the stream
    // we are not reading yet — which is exactly what a build tool does.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    let mut drains = DrainTasks(vec![
        tokio::spawn(drain_pipe(stdout, sink.clone(), Some(out_tx))),
        tokio::spawn(drain_pipe(stderr, sink, None)),
    ]);

    // The step is over when *its* process exits, not when the last descendant
    // that inherited these pipes closes them — see [`DRAIN_GRACE`]. The readers
    // keep running while we wait, so the child can never block on a full pipe.
    let status = child.wait().await.map_err(ProcessError::SpawnFailed)?;
    let drained = tokio::time::timeout(DRAIN_GRACE, async {
        for task in drains.0.iter_mut() {
            let _ = task.await;
        }
    })
    .await;
    if drained.is_err() {
        tracing::warn!(
            command = command.display(),
            "step exited but something it started still holds its output pipes — \
             stopped collecting its output"
        );
    }
    // Whether the drain finished, timed out, or this whole future was cancelled,
    // the tasks stop here.
    drop(drains);

    let mut outputs = HashMap::new();
    while let Ok((key, value)) = out_rx.try_recv() {
        outputs.insert(key, value);
    }

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
        tracing::warn!(
            exit_code,
            command = command.display(),
            "command step exited with non-zero code"
        );
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
/// The line splitting here is `\n`-only and the drain is unbounded, unlike
/// [`run_command`]: this path echoes to a terminal that was never taken away
/// from it, and its child is one veld owns the interrupt handling for. Those
/// are the two differences to weigh before sharing code between them.
///
/// The child runs in its own process group so a Ctrl+C delivered to the CLI's
/// controlling terminal is not auto-forwarded to it; instead we catch the
/// signal, kill the whole group, and report exit code `130` (SIGINT). This
/// keeps interruption deterministic regardless of how the child handles
/// signals itself.
/// `on_spawn` is the same loan [`run_command_observed`] hands out, for the same
/// reason: a `--oneshot` terminal node is often the longest and heaviest command
/// in a run (an end-to-end suite), and it is spawned here rather than through
/// `run_command`, so without this it would be the one `command` node nothing
/// could measure.
pub async fn run_command_streaming(
    command: &CommandSpec,
    working_dir: &Path,
    env: &HashMap<String, String>,
    output_file: Option<&Path>,
    log_target: Option<LogTarget>,
    on_spawn: Option<Box<dyn FnOnce(u32) + Send>>,
) -> Result<CommandOutput, ProcessError> {
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

    let spawn_result = match tokio_command(command) {
        Ok(mut cmd) => cmd
            .current_dir(working_dir)
            .envs(&env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0) // own group — we forward Ctrl+C by killing the group
            .spawn(),
        Err(e) => {
            if let Some(path) = output_file {
                let _ = std::fs::remove_file(path);
            }
            return Err(e);
        }
    };

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
    // `unwrap_or(0)` above is this path's pre-existing convention for the kill
    // group; 0 is not a PID a sampler may walk, so the observer is told only
    // about a real one.
    if let (Some(on_spawn), true) = (on_spawn, pid != 0) {
        on_spawn(pid);
    }
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
        drain_to_terminal_and_log(stdout, tokio::io::stdout(), out_log, Some(out_tx)).await;
    });
    let err_log = log_target.clone();
    let err_task = tokio::spawn(async move {
        drain_to_terminal_and_log(stderr, tokio::io::stderr(), err_log, None).await;
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
///
/// **Pid 0 is not a process and is answered `false` without asking the kernel.**
/// `kill(0, …)` addresses the *caller's own process group*, so it succeeds
/// unconditionally — which made `is_alive(0)` return `true` from inside every
/// process that asked. Nothing in veld stores 0 to mean "this pid", it means "no
/// process", and reading it as alive is how a placeholder becomes a live
/// process: a corrupt state file claiming pid 0 held the update lock would have
/// been believed. `wait_for_pid_exit` has carried the same guard for the same
/// reason; this is that reasoning stated where the check actually lives.
/// Anything above `i32::MAX` cannot round-trip through `Pid` either, and would
/// otherwise be truncated into some *other* live process.
///
/// **Only `ESRCH` means dead.** `EPERM` means the process exists and belongs to
/// somebody this user may not signal, which is the opposite answer — and the one
/// this function used to give, because every error collapsed into `false`. It
/// bites wherever veld reads a pid it did not spawn: a root-owned process, or one
/// belonging to another account on a shared machine, was reported *gone* — and
/// for the update lock that means a live holder judged abandoned and its lock
/// stolen. `desktop/src/updater.js` has always read `EPERM` as alive, so this
/// also stops the two halves of the same product answering the same question
/// differently. Any other errno is unexpected and reads as dead, which is what
/// the previous behaviour was for every case.
pub fn is_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    match kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
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

// ---------------------------------------------------------------------------
// RC1 characterization tests
// ---------------------------------------------------------------------------

/// Characterization tests for the **detached** spawn path — the process
/// topology every later change must preserve.
///
/// These deliberately assert *today's* behaviour rather than a desired one.
/// The detached wrapper is a shell pipeline by construction
/// (`sh -c '{ cmd ; } 2>&1 | veld _log …'`), and a great deal of veld depends on
/// its exact shape: the tracked PID is the pipeline shell and the process-group
/// leader, so `kill(-pid)` reaps the node *and* the log writer; `is_alive` on
/// that PID is what `cleanup_dead_runs` and the daemon monitor treat as "the
/// node is up"; and the daemon's stats sampler walks the parent tree from it.
///
/// If a refactor of the spawn path changes any assertion here, the refactor is
/// wrong — not the test.
#[cfg(test)]
mod characterization_tests {
    use super::*;

    /// Live PIDs in the process group `pgid`, via `ps` (portable across macOS
    /// and Linux; `sysinfo` does not expose process groups).
    ///
    /// Zombies are excluded. In production the CLI exits and the pipeline is
    /// reparented to init/launchd, which reaps it; under a test the test binary
    /// *is* the parent, so an exited stage lingers as a zombie that `kill(pid,
    /// 0)` still reports as alive. That is an artefact of the test harness, not
    /// of the behaviour under test.
    fn pids_in_group(pgid: u32) -> Vec<u32> {
        let out = std::process::Command::new("ps")
            .args(["-eo", "pid=,pgid=,state="])
            .output()
            .expect("ps must be available");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut f = line.split_whitespace();
                let pid: u32 = f.next()?.parse().ok()?;
                let gid: u32 = f.next()?.parse().ok()?;
                let state = f.next().unwrap_or("");
                (gid == pgid && !state.starts_with('Z')).then_some(pid)
            })
            .collect()
    }

    fn group_of(pid: u32) -> u32 {
        use nix::unistd::{Pid, getpgid};
        getpgid(Some(Pid::from_raw(pid as i32)))
            .expect("process must still exist")
            .as_raw() as u32
    }

    /// Reap the pipeline shell if it has exited, so `is_alive` stops reporting a
    /// zombie as running. See [`pids_in_group`] for why this is only needed in
    /// tests: `spawn_detached` deliberately drops the `Child` handle because in
    /// production the CLI exits and init takes over reaping.
    fn reap(pid: u32) {
        use nix::sys::wait::{WaitPidFlag, waitpid};
        let _ = waitpid(
            nix::unistd::Pid::from_raw(pid as i32),
            Some(WaitPidFlag::WNOHANG),
        );
    }

    /// Whether the pipeline shell has exited (reaping it first).
    fn exited(pid: u32) -> bool {
        reap(pid);
        !is_alive(pid)
    }

    /// Poll `cond` until it holds or `secs` elapse. Returns whether it held.
    async fn eventually(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..(secs * 20) {
            if cond() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        cond()
    }

    /// A stub log sink: a single binary that drains stdin into a file, standing
    /// in for `veld _log` (whose binary is not at a test process's
    /// `current_exe()`). One exec, like the real sink, so the process topology
    /// matches production.
    fn sink_argv(path: &Path) -> Vec<String> {
        vec!["tee".to_owned(), path.to_string_lossy().into_owned()]
    }

    /// The tracked PID is the pipeline's process-group leader, and killing the
    /// group reaps both the node process and the log writer. `kill_process`
    /// signals `-pid`, so this property is what makes `veld stop` complete
    /// rather than leaving an orphaned log writer holding the pipe.
    #[tokio::test]
    async fn tracked_pid_is_group_leader() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("sink.log");
        let handle = spawn_detached(
            &CommandSpec::Shell("sleep 30".to_owned()),
            tmp.path(),
            &HashMap::new(),
            &sink_argv(&log),
        )
        .expect("detached spawn");
        let pid = handle.pid();

        // The tracked PID leads its own group (process_group(0) on the spawn).
        assert_eq!(group_of(pid), pid, "tracked pid must be the group leader");

        // Both pipeline stages live in that group, so a group signal covers
        // the log writer too — not just the node.
        assert!(
            eventually(5, || pids_in_group(pid).len() >= 2).await,
            "pipeline should have at least the shell and one stage in the group, saw {:?}",
            pids_in_group(pid)
        );

        kill_process(pid).await.expect("kill the group");
        assert!(
            eventually(5, || pids_in_group(pid).is_empty()).await,
            "killing the group must reap every stage, still alive: {:?}",
            pids_in_group(pid)
        );
        assert!(exited(pid));
    }

    /// A server that double-forks and lets its direct child exit is NOT
    /// observable as dead: the daemonized grandchild keeps the stdout pipe
    /// open, so the log writer never sees EOF and the pipeline shell (the
    /// tracked PID) keeps waiting on it.
    ///
    /// This is why `cleanup_dead_runs` does not reap such a run — see
    /// `is_reapable_orphan`, which is only ever reached with `any_alive =
    /// false`. Rebuilding the pipeline with real file descriptors would make
    /// the tracked PID exit here and silently start reaping live environments.
    #[tokio::test]
    async fn double_forking_server_not_reaped() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("sink.log");
        // `( … & )` backgrounds in a subshell that exits immediately: the
        // direct child is gone but `sleep` inherited the pipe.
        let handle = spawn_detached(
            &CommandSpec::Shell("( sleep 30 & ) ; exit 0".to_owned()),
            tmp.path(),
            &HashMap::new(),
            &sink_argv(&log),
        )
        .expect("detached spawn");
        let pid = handle.pid();

        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        assert!(
            !exited(pid),
            "tracked pid must stay alive while a daemonized descendant holds \
             the log pipe — otherwise cleanup_dead_runs reaps a live run"
        );
        // Consequently `cleanup_dead_runs` sees `any_alive = true` and
        // `is_reapable_orphan` (unit-tested in `orchestrator`) returns false
        // for every status — the run is left alone.

        kill_process(pid).await.expect("kill the group");
        assert!(eventually(5, || pids_in_group(pid).is_empty()).await);
    }

    /// **The argv guarantee, through the detached path.**
    ///
    /// Interpolated values containing spaces, `?`, `*`, `$`, quotes, and a newline
    /// must each yield exactly one argv element. This has to exercise the detached
    /// wrapper, not just `CommandSpec::interpolate`: the wrapper is a shell
    /// pipeline, so a version that re-joined argv into the script would pass a
    /// pure-function test and then let the shell re-split every one of these in
    /// production. `"$@"` is what makes it hold.
    #[tokio::test]
    async fn interpolation_never_changes_argc() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("argv.txt");

        // Each of these would become two or more words, or expand, under a shell.
        let hostile = [
            "two words",
            "quest?ion",
            "glob*star",
            "$HOME",
            "has'single",
            "has\"double",
            "semi;colon",
            "pipe|bar",
            "new\nline",
            "", // an empty argument must survive as an empty argument
        ];

        // Substitute them through the real interpolation path, so the test covers
        // "value came from a variable" rather than a hand-written literal.
        let mut ctx = crate::variables::VariableContext::new();
        let mut argv = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            // `$#` is the argument count as the *shell* sees it, after `"$@"`.
            format!("printf '%s\\n' \"$#\" > '{}'", out.display()),
            "sh".to_owned(), // $0 for the inner shell
        ];
        for (i, value) in hostile.iter().enumerate() {
            ctx.set_builtin(&format!("v{i}"), (*value).to_owned());
            argv.push(format!("${{veld.v{i}}}"));
        }

        let spec = CommandSpec::Argv(argv)
            .interpolate(&ctx)
            .expect("interpolation resolves");
        // The element count is fixed before substitution, so it is already right.
        assert_eq!(
            spec_len(&spec),
            4 + hostile.len(),
            "interpolation must not change the element count"
        );

        let handle = spawn_detached(
            &spec,
            tmp.path(),
            &HashMap::new(),
            &sink_argv(&tmp.path().join("sink.log")),
        )
        .expect("detached spawn");
        let pid = handle.pid();
        assert!(
            eventually(10, || exited(pid)).await,
            "pipeline should finish"
        );

        let seen: usize = std::fs::read_to_string(&out)
            .expect("inner shell wrote its argument count")
            .trim()
            .parse()
            .expect("a number");
        assert_eq!(
            seen,
            hostile.len(),
            "every hostile value must arrive as exactly one argument, even through \
             the detached shell pipeline"
        );
    }

    fn spec_len(spec: &CommandSpec) -> usize {
        match spec {
            CommandSpec::Argv(a) => a.len(),
            CommandSpec::Shell(_) => 1,
        }
    }

    /// A `shell` node keeps the exact wrapper it had before argv existed, and an
    /// `argv` node gets the positional-parameter form. Pins the emitted script,
    /// which no other test covers — renaming a `veld _log` flag would otherwise
    /// only fail at runtime.
    #[test]
    fn detached_wrapper_shape_is_pinned() {
        // A directory of its own, not a fixed name in `temp_dir()`. That path is
        // shared by every checkout on the machine, so a second worktree whose
        // branch adds a migration leaves a database this branch reads as
        // `NewerSchema { found: 7, supported: 6 }` — a failure with nothing to do
        // with the test, in a suite that is green in CI (where the temp dir is
        // always fresh) and red locally.
        let dir = tempfile::tempdir().unwrap();
        let target = LogTarget {
            db: Db::open_at(&dir.path().join("wrapper.db")).unwrap(),
            project_root: PathBuf::from("/pro ject"),
            run_name: "dev".into(),
            run_id: "rid".into(),
            node: "api".into(),
            variant: "local".into(),
        };
        let sink = log_sink_argv(&target);
        assert_eq!(
            &sink[1..],
            &[
                "_log",
                "--project-root",
                "/pro ject",
                "--run",
                "dev",
                "--run-id",
                "rid",
                "--node",
                "api",
                "--variant",
                "local",
            ],
            "the _log flag names and order are what the detached pipeline depends on"
        );
        // A path containing a single quote must not break out of the script.
        let quoted = sq("it's");
        assert_eq!(quoted, "it'\\''s");
    }

    /// End-to-end log capture: `2>&1` in the wrapper merges the node's stderr
    /// into its stdout, both reach the log sink, and the sink sees EOF (exits)
    /// once the node does.
    #[tokio::test]
    async fn detached_logs_reach_run_log() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("sink.log");
        let handle = spawn_detached(
            &CommandSpec::Shell("echo to-stdout; echo to-stderr 1>&2".to_owned()),
            tmp.path(),
            &HashMap::new(),
            &sink_argv(&log),
        )
        .expect("detached spawn");
        let pid = handle.pid();

        // The whole pipeline exits on its own: the node finishes, the sink
        // reaches EOF, the pipeline shell reaps it.
        assert!(
            eventually(10, || exited(pid)).await,
            "pipeline must reach EOF and exit once the node exits"
        );

        let captured = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            captured.contains("to-stdout"),
            "stdout must reach the log sink, got {captured:?}"
        );
        assert!(
            captured.contains("to-stderr"),
            "stderr must reach the log sink via 2>&1, got {captured:?}"
        );
    }
}

#[cfg(test)]
mod liveness_tests {
    use super::*;

    #[test]
    fn a_pid_that_is_not_a_process_is_never_alive() {
        // The one that bit: `kill(0, …)` signals the caller's own process group,
        // so without the guard this returns `true` from inside any process — and
        // 0 is what every "no process here" placeholder in veld deserialises to.
        assert!(!is_alive(0));
        // Truncating into an `i32` would otherwise turn this into some unrelated
        // live pid.
        assert!(!is_alive(i32::MAX as u32 + 1));
        assert!(!is_alive(u32::MAX));
        // The control: this process is alive, so the guard has not simply
        // disabled the check.
        assert!(is_alive(std::process::id()));
    }

    #[test]
    fn a_process_this_user_may_not_signal_is_alive_not_dead() {
        // pid 1 (launchd / init) is always running and always owned by root, so
        // an unprivileged test process gets EPERM for it — the one errno that
        // means "it exists" while not being `Ok`. Collapsing it into "dead" is
        // what let an unprivileged veld try to steal a root-held update lock.
        // Skipped when the tests happen to run as root, where the answer is `Ok`
        // and the case is unreachable rather than wrong.
        if nix::unistd::Uid::effective().is_root() {
            return;
        }
        assert!(is_alive(1), "pid 1 exists; EPERM is not ESRCH");
    }
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
            &CommandSpec::Shell(
                "echo hello; echo 'VELD_OUTPUT foo=bar'; echo oops 1>&2; exit 3".to_owned(),
            ),
            &dir,
            &env,
            None,
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
        let out = run_command_streaming(
            &CommandSpec::Shell("true".to_owned()),
            &dir,
            &env,
            None,
            None,
            None,
        )
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
            &CommandSpec::Shell("printf '\\377\\n'; echo 'VELD_OUTPUT foo=bar'".to_owned()),
            &dir,
            &env,
            None,
            None,
            None,
        )
        .await
        .expect("streaming run should tolerate non-UTF-8 output");
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.outputs.get("foo").map(String::as_str), Some("bar"));
    }
}

#[cfg(test)]
mod command_capture_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Collect every line a sink is handed.
    fn recording_sink() -> (LineSink, Arc<Mutex<Vec<String>>>) {
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_lines = Arc::clone(&lines);
        let sink: LineSink = Arc::new(move |batch: &[String]| {
            sink_lines.lock().unwrap().extend_from_slice(batch);
        });
        (sink, lines)
    }

    /// The regression this whole path exists for: a `command` step's output —
    /// on *both* pipes — reaches the sink. stderr used to be inherited straight
    /// onto the terminal and stdout was read only for control lines, so a
    /// `docker build` left nothing behind for `veld logs`.
    #[tokio::test]
    async fn captures_stdout_and_stderr() {
        let (sink, lines) = recording_sink();
        let out = run_command(
            &CommandSpec::Shell("echo to-stdout; echo to-stderr >&2".to_owned()),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(sink),
        )
        .await
        .expect("run should succeed");

        assert_eq!(out.exit_code, 0);
        let lines = lines.lock().unwrap().clone();
        assert!(lines.contains(&"to-stdout".to_owned()), "got {lines:?}");
        assert!(lines.contains(&"to-stderr".to_owned()), "got {lines:?}");
    }

    /// `VELD_OUTPUT` control lines are machinery, not output: they are parsed
    /// into `outputs` and must never reach the sink, or a value the author
    /// deliberately kept off the terminal lands in the log instead.
    #[tokio::test]
    async fn control_lines_are_parsed_but_never_sinked() {
        let (sink, lines) = recording_sink();
        let out = run_command(
            &CommandSpec::Shell("echo 'VELD_OUTPUT token=s3cret'; echo visible".to_owned()),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(sink),
        )
        .await
        .expect("run should succeed");

        assert_eq!(out.outputs.get("token").map(String::as_str), Some("s3cret"));
        let lines = lines.lock().unwrap().clone();
        assert_eq!(lines, vec!["visible".to_owned()]);
    }

    /// The same peel on **stderr**, where it matters more: stderr is not an
    /// output channel, so a control line there is not a declared output — it is
    /// a `set -x` trace, or a script echoing to the wrong stream. Parsing it
    /// would invent an output; sinking it would put the value in the log the
    /// file channel exists to keep it out of.
    #[tokio::test]
    async fn control_lines_on_stderr_are_dropped_not_parsed() {
        let (sink, lines) = recording_sink();
        let out = run_command(
            &CommandSpec::Shell(
                "echo 'VELD_OUTPUT token=s3cret' >&2; echo 'real error' >&2".to_owned(),
            ),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(sink),
        )
        .await
        .expect("run should succeed");

        assert!(out.outputs.is_empty(), "stderr is not an output channel");
        let lines = lines.lock().unwrap().clone();
        assert_eq!(lines, vec!["real error".to_owned()]);
    }

    /// A step that backgrounds something leaves a descendant holding the write
    /// end of both pipes, so they never reach EOF. The step is over when its own
    /// process exits — waiting for the pipes wedged the CLI for as long as the
    /// survivor lived.
    #[tokio::test]
    async fn backgrounded_grandchild_does_not_wedge_the_step() {
        let (sink, lines) = recording_sink();
        let started = std::time::Instant::now();
        let out = tokio::time::timeout(
            DRAIN_GRACE + std::time::Duration::from_secs(8),
            run_command(
                &CommandSpec::Shell("sleep 30 & echo done".to_owned()),
                &std::env::temp_dir(),
                &HashMap::new(),
                None,
                Some(sink),
            ),
        )
        .await
        .expect("must not wait for the backgrounded process")
        .expect("run should succeed");

        assert_eq!(out.exit_code, 0);
        assert!(
            started.elapsed() < DRAIN_GRACE + std::time::Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        // Output printed before the child exited still made it through.
        assert!(lines.lock().unwrap().contains(&"done".to_owned()));
    }

    /// A progress meter that redraws with a bare `\r` never emits a newline. A
    /// `\n`-only reader accumulates its whole run in memory and stores it as one
    /// row; each redraw is its own line. `\r\n` still counts once.
    #[tokio::test]
    async fn carriage_returns_split_lines() {
        let (sink, lines) = recording_sink();
        run_command(
            &CommandSpec::Shell("printf '10%%\\r50%%\\r100%%\\r\\ndone\\n'".to_owned()),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(sink),
        )
        .await
        .expect("run should succeed");

        let lines = lines.lock().unwrap().clone();
        assert_eq!(
            lines,
            vec![
                "10%".to_owned(),
                "50%".to_owned(),
                "100%".to_owned(),
                "done".to_owned()
            ],
        );
    }

    /// A single line with no break at all is capped rather than buffered
    /// without bound, and the tail that never got a newline is still sinked.
    #[tokio::test]
    async fn unterminated_output_is_capped_and_still_sinked() {
        let (sink, lines) = recording_sink();
        let bytes = MAX_LINE_BYTES + 100;
        run_command(
            &CommandSpec::Shell(format!("printf 'x%.0s' $(seq 1 {bytes})")),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(sink),
        )
        .await
        .expect("run should succeed");

        let lines = lines.lock().unwrap().clone();
        assert_eq!(lines.len(), 2, "capped into two lines, got {}", lines.len());
        assert_eq!(lines[0].len(), MAX_LINE_BYTES);
        assert_eq!(lines[1].len(), 100);
    }

    /// Cancelling the call stops the pipe readers.
    ///
    /// The sink holds whatever the caller wired into it — for the orchestrator,
    /// a clone of the progress channel's sender. A reader that outlives its
    /// `run_command` keeps that sender alive, and the CLI's progress task ends
    /// only when the last sender drops: Ctrl+C during a `command` node would
    /// hang `veld start` instead of tearing the run down, with the step's
    /// process still running and its pipes still open.
    #[tokio::test]
    async fn cancelling_the_call_releases_what_the_sink_holds() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let sink: LineSink = Arc::new(move |batch: &[String]| {
            for line in batch {
                let _ = tx.send(line.clone());
            }
        });

        let call = tokio::spawn(async move {
            // Long-lived: still running when the call is cancelled.
            let _ = run_command(
                &CommandSpec::Shell("echo hi; sleep 60".to_owned()),
                &std::env::temp_dir(),
                &HashMap::new(),
                None,
                Some(sink),
            )
            .await;
        });

        // Let the step start and print, so the readers are live.
        assert_eq!(rx.recv().await.as_deref(), Some("hi"));
        call.abort();

        // The channel must close (every sender dropped), not stall.
        let closed = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .expect("sender clones must be released when the call is cancelled");
        assert!(closed.is_none(), "expected the channel to close");
    }

    /// A `\r` that ends no line is a meter redraw, not a blank line — but a
    /// blank line the step really printed survives, in either line ending.
    ///
    /// The two halves pull against each other: `\n\r` must not produce a blank
    /// row, while `\r\n\r\n` must, and only the byte after the `\r` tells them
    /// apart.
    #[tokio::test]
    async fn blank_lines_survive_but_meter_redraws_do_not() {
        for (script, expected) in [
            // LF: a redraw after a break, then a real blank line.
            ("printf 'a\\n\\rb\\n\\n c\\n'", vec!["a", "b", "", " c"]),
            // CRLF: the blank line between two paragraphs is real.
            ("printf 'a\\r\\n\\r\\nb\\r\\n'", vec!["a", "", "b"]),
            // A meter redrawing, CRLF-terminated: three states, no blank.
            (
                "printf '10%%\\r50%%\\r100%%\\r\\n'",
                vec!["10%", "50%", "100%"],
            ),
            // Consecutive redraws collapse rather than emitting empties.
            ("printf '50%%\\r\\r100%%\\n'", vec!["50%", "100%"]),
        ] {
            let (sink, lines) = recording_sink();
            run_command(
                &CommandSpec::Shell(script.to_owned()),
                &std::env::temp_dir(),
                &HashMap::new(),
                None,
                Some(sink),
            )
            .await
            .expect("run should succeed");

            let lines = lines.lock().unwrap().clone();
            assert_eq!(lines, expected, "for {script}");
        }
    }

    /// The length cap must not cut a multi-byte character in half: that would
    /// corrupt one character on each side of a break the step never asked for.
    #[test]
    fn the_length_cap_retreats_to_a_character_boundary() {
        // "é" is two bytes; a buffer ending on its lead byte holds one back.
        assert_eq!(trailing_partial_char("aé".as_bytes()), 0);
        assert_eq!(trailing_partial_char(&"aé".as_bytes()[..2]), 1);
        // Three-byte "€", cut after one and after two bytes.
        assert_eq!(trailing_partial_char(&"€".as_bytes()[..1]), 1);
        assert_eq!(trailing_partial_char(&"€".as_bytes()[..2]), 2);
        assert_eq!(trailing_partial_char("€".as_bytes()), 0);
        // Invalid input holds nothing back rather than stalling.
        assert_eq!(trailing_partial_char(&[0xFF]), 0);
        assert_eq!(trailing_partial_char(b""), 0);
    }

    /// stdin is `/dev/null`: a step that reads it gets EOF immediately instead
    /// of blocking on a prompt that, with both pipes captured, nobody can see.
    #[tokio::test]
    async fn stdin_is_closed_so_a_prompt_cannot_hang() {
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_command(
                &CommandSpec::Shell("read -r answer; echo \"got:${answer:-nothing}\"".to_owned()),
                &std::env::temp_dir(),
                &HashMap::new(),
                None,
                None,
            ),
        )
        .await
        .expect("must not block waiting for input")
        .expect("run should succeed");

        // `read` hits EOF and returns non-zero; the point is that it returns.
        assert_ne!(out.exit_code, -1);
    }

    /// Both pipes are drained concurrently. Reading them in sequence deadlocks
    /// as soon as a step writes more than a pipe buffer (~64 KiB) to the stream
    /// that is not being read — which is what a build tool does.
    #[tokio::test]
    async fn large_output_on_both_pipes_does_not_deadlock() {
        let (sink, lines) = recording_sink();
        let script = "i=0; while [ $i -lt 4000 ]; do \
                      echo \"out-$i\"; echo \"err-$i\" >&2; i=$((i+1)); done";
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            run_command(
                &CommandSpec::Shell(script.to_owned()),
                &std::env::temp_dir(),
                &HashMap::new(),
                None,
                Some(sink),
            ),
        )
        .await
        .expect("must not deadlock on a full pipe buffer")
        .expect("run should succeed");

        assert_eq!(out.exit_code, 0);
        assert_eq!(lines.lock().unwrap().len(), 8000);
    }

    /// A non-UTF-8 byte costs one character, not the rest of the stream.
    #[tokio::test]
    async fn lossy_decode_survives_non_utf8() {
        let (sink, lines) = recording_sink();
        let out = run_command(
            &CommandSpec::Shell("printf '\\377\\n'; echo after".to_owned()),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(sink),
        )
        .await
        .expect("run should tolerate non-UTF-8 output");

        assert_eq!(out.exit_code, 0);
        assert!(lines.lock().unwrap().contains(&"after".to_owned()));
    }
}

#[cfg(test)]
mod batched_drain_tests {
    use super::*;
    use crate::db::{LogFilter, LogStream};

    /// A `LogTarget` backed by a real temp database, so the batched drains can
    /// be asserted on what actually landed in `log_lines`.
    ///
    /// `Db::open_at` rather than the shared `test_db()` helper, for the reason
    /// the wrapper test above gives: an explicit fresh path cannot collide with
    /// another test's schema version.
    fn temp_target() -> (tempfile::TempDir, LogTarget) {
        let dir = tempfile::tempdir().unwrap();
        let target = LogTarget {
            db: Db::open_at(&dir.path().join("drain.db")).unwrap(),
            project_root: PathBuf::from("/proj"),
            run_name: "dev".into(),
            run_id: "rid".into(),
            node: "api".into(),
            variant: "local".into(),
        };
        (dir, target)
    }

    fn stored(target: &LogTarget) -> Vec<String> {
        target
            .db
            .tail_logs(
                &target.project_root,
                &target.run_name,
                &LogFilter::default(),
                10_000,
            )
            .unwrap()
            .into_iter()
            .map(|r| r.line)
            .collect()
    }

    /// Every line a server prints lands exactly once, in order, through the
    /// batched path — including the last one, and including one that never got
    /// its newline.
    ///
    /// The batching is invisible from here, which is the point: the flush cap and
    /// the flush deadline are both internal, and no reader is supposed to be able
    /// to tell how the rows were grouped into transactions.
    #[tokio::test]
    async fn log_pipe_stores_every_line_including_an_unterminated_last_one() {
        let (_dir, target) = temp_target();
        // 1,200 lines crosses `LOG_BATCH_MAX_LINES` (512) twice, so the size cap
        // fires and the remainder leaves a partial batch for the EOF flush.
        let mut payload = String::new();
        for i in 0..1_200 {
            payload.push_str(&format!("line {i}\n"));
        }
        payload.push_str("no trailing newline");

        log_pipe(std::io::Cursor::new(payload.into_bytes()), target.clone()).await;

        let rows = stored(&target);
        assert_eq!(rows.len(), 1_201, "every line must land exactly once");
        assert_eq!(rows[0], "line 0");
        assert_eq!(rows[1_199], "line 1199");
        assert_eq!(
            rows[1_200], "no trailing newline",
            "a final line with no newline is still output and must be stored"
        );
    }

    /// The regression that the batching introduced and that this test now pins:
    /// a trailing line with no newline, printed after a pause long enough for the
    /// flush deadline to fire, used to be **lost from both the terminal and the
    /// database**.
    ///
    /// The pause is what makes it a regression rather than a hypothetical. It
    /// lets the deadline cancel a `read_until` that has already appended the
    /// partial bytes into the caller's buffer; the next call then reports `Ok(0)`
    /// at EOF (tokio resets its per-future byte count), and the old `Ok(0) =>
    /// break` dropped the buffer on the floor. Before the batching there was no
    /// deadline and `buf` was cleared every iteration, so this state could not
    /// arise at all.
    #[tokio::test]
    async fn a_trailing_line_after_a_flush_deadline_is_not_lost() {
        let (_dir, target) = temp_target();
        let out = run_command_streaming(
            // **Order matters and is the whole repro.** The partial line must
            // be written *before* the pause, so `read_until` has already
            // appended its bytes into `buf` when the 50 ms deadline cancels it;
            // the pause then holds the pipe open with nothing more to read, so
            // the next call reaches EOF and reports `Ok(0)`. Put the sleep
            // between the two writes instead and the cancelled read appended
            // nothing, the partial arrives on a fresh `Ok(n>0)`, and the test
            // passes against the very bug it is written for — which this one did
            // until it was checked by re-introducing the defect.
            &CommandSpec::Shell("printf 'first\\npartial-no-newline'; sleep 0.4".to_owned()),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(target.clone()),
            None,
        )
        .await
        .expect("run should succeed");
        assert_eq!(out.exit_code, 0);

        let rows = stored(&target);
        assert!(
            rows.contains(&"first".to_owned()),
            "the terminated line must be stored; got {rows:?}"
        );
        assert!(
            rows.contains(&"partial-no-newline".to_owned()),
            "the unterminated final line must survive the flush deadline; got {rows:?}"
        );
    }

    /// Same for stderr, which is a second copy of the same loop.
    #[tokio::test]
    async fn a_trailing_stderr_line_after_a_flush_deadline_is_not_lost() {
        let (_dir, target) = temp_target();
        run_command_streaming(
            &CommandSpec::Shell("printf 'err-first\\nerr-partial' >&2; sleep 0.4".to_owned()),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(target.clone()),
            None,
        )
        .await
        .expect("run should succeed");

        let rows = stored(&target);
        assert!(
            rows.contains(&"err-partial".to_owned()),
            "the unterminated final stderr line must survive; got {rows:?}"
        );
    }

    /// **The bound, on the reader that had none.** `log_pipe` used
    /// `BufReader::lines()`, which does not return until it sees a `\n` — so a
    /// foreground server emitting a `\r`-only progress meter accumulated its
    /// entire output as one row, and `MAX_LINE_BYTES` could not be applied from
    /// outside because there was no point at which to apply it.
    #[tokio::test]
    async fn log_pipe_bounds_a_stream_that_never_sends_a_newline() {
        let (_dir, target) = temp_target();
        let mut payload = Vec::new();
        for i in 0..20_000 {
            payload.extend_from_slice(format!("progress {i}\r").as_bytes());
        }
        let raw_len = payload.len();
        assert!(
            raw_len > MAX_LINE_BYTES * 3,
            "the fixture must exceed the cap several times"
        );

        log_pipe(std::io::Cursor::new(payload), target.clone()).await;

        let rows = stored(&target);
        assert!(
            rows.len() > 1,
            "an uncapped reader would have stored exactly one row"
        );
        assert!(
            rows.iter().all(|l| l.len() <= MAX_LINE_BYTES),
            "no row may exceed the cap"
        );
        assert_eq!(
            rows.iter().map(String::len).sum::<usize>(),
            raw_len - 1,
            "splitting must lose nothing but the one trailing terminator"
        );
    }

    /// Same bound, on `run_command_streaming`'s drains — which had the same
    /// unbounded shape via `read_until`, on the path a `docker build`'s progress
    /// output takes.
    #[tokio::test]
    async fn a_command_steps_progress_meter_is_bounded_on_both_pipes() {
        for pipe in ["", " >&2"] {
            let (_dir, target) = temp_target();
            run_command_streaming(
                &CommandSpec::Shell(format!(
                    "python3 -c \"
import sys
for i in range(20000): sys.stdout.write('progress %06d\\r'%i)
\"{pipe}"
                )),
                &std::env::temp_dir(),
                &HashMap::new(),
                None,
                Some(target.clone()),
                None,
            )
            .await
            .expect("run should succeed");

            let rows = stored(&target);
            assert!(
                rows.len() > 1,
                "an uncapped drain stored one row for pipe {pipe:?}"
            );
            assert!(
                rows.iter().all(|l| l.len() <= MAX_LINE_BYTES),
                "no row may exceed the cap on pipe {pipe:?}"
            );
        }
    }

    /// `VELD_OUTPUT` control lines are machinery and must never reach the log,
    /// batched or not — they can carry sensitive node outputs.
    #[tokio::test]
    async fn control_lines_are_not_stored_by_the_batched_drain() {
        let (_dir, target) = temp_target();
        let out = run_command_streaming(
            &CommandSpec::Shell("echo VELD_OUTPUT token=s3cr3t; echo ordinary".to_owned()),
            &std::env::temp_dir(),
            &HashMap::new(),
            None,
            Some(target.clone()),
            None,
        )
        .await
        .expect("run should succeed");
        assert_eq!(out.outputs.get("token").map(String::as_str), Some("s3cr3t"));

        let rows = stored(&target);
        assert!(rows.contains(&"ordinary".to_owned()), "got {rows:?}");
        assert!(
            !rows.iter().any(|l| l.contains("s3cr3t")),
            "a VELD_OUTPUT control line must never be logged; got {rows:?}"
        );
    }

    /// The `internal` stream is where veld's own words go, and the batched
    /// drains must not put a supervised process's output there — that boundary
    /// is what lets a reader tell veld's claims from the child's.
    #[tokio::test]
    async fn a_processs_output_never_reaches_the_internal_stream() {
        let (_dir, target) = temp_target();
        log_pipe(
            std::io::Cursor::new(b"[log] dropped 5 lines\n".to_vec()),
            target.clone(),
        )
        .await;

        let internal = target
            .db
            .tail_logs(
                &target.project_root,
                &target.run_name,
                &LogFilter {
                    streams: Some(vec![LogStream::Internal.as_str()]),
                    ..Default::default()
                },
                100,
            )
            .unwrap();
        assert!(
            internal.is_empty(),
            "a line printed by the supervised process must stay on its own stream, \
             whatever it says: {internal:?}"
        );
    }
}

#[cfg(test)]
mod line_assembler_tests {
    use super::{LineAssembler, MAX_LINE_BYTES};

    fn feed(chunks: &[&[u8]]) -> Vec<String> {
        let mut a = LineAssembler::new();
        let mut out = Vec::new();
        for c in chunks {
            a.push_chunk(c, &mut out);
        }
        out.extend(a.finish());
        out
    }

    /// The semantics the three readers this replaces already had.
    #[test]
    fn line_endings_match_what_the_readers_it_replaces_did() {
        assert_eq!(feed(&[b"a\nb\n"]), vec!["a", "b"]);
        assert_eq!(
            feed(&[b"a\r\nb\r\n"]),
            vec!["a", "b"],
            "CRLF stores neither"
        );
        assert_eq!(
            feed(&[b"a\rb\n"]),
            vec!["a\rb"],
            "an embedded CR is data — the renderer collapses it to the last frame"
        );
        assert_eq!(feed(&[b"x\r\r\n"]), vec!["x"], "all trailing CRs go");
        assert_eq!(feed(&[b"\n\n"]), vec!["", ""], "blank lines are lines");
    }

    /// Output that never got its newline is still output.
    #[test]
    fn a_final_line_without_a_newline_is_emitted() {
        assert_eq!(feed(&[b"a\nlast-no-newline"]), vec!["a", "last-no-newline"]);
        assert_eq!(feed(&[b"only"]), vec!["only"]);
    }

    /// EOF with nothing but terminators emits no row. A blank line always arrives
    /// with its `\n`, so this cannot swallow one.
    #[test]
    fn eof_with_only_terminators_emits_nothing() {
        assert_eq!(feed(&[b"a\n", b"\r"]), vec!["a"]);
        assert_eq!(feed(&[b""]), Vec::<String>::new());
    }

    /// **The bound, which is the whole reason this type exists.** A stream with no
    /// newline at all must not accumulate: `Lines::next_line` and `read_until`
    /// would have held all of it in memory.
    #[test]
    fn a_stream_with_no_newline_is_split_at_the_cap_and_loses_nothing() {
        let raw = vec![b'A'; MAX_LINE_BYTES * 3 + 17];
        let lines = feed(&[&raw]);
        assert_eq!(lines.len(), 4, "3 full lines plus the remainder");
        for l in &lines[..3] {
            assert_eq!(
                l.len(),
                MAX_LINE_BYTES,
                "each split lands exactly on the cap"
            );
        }
        assert_eq!(lines[3].len(), 17);
        assert_eq!(
            lines.iter().map(String::len).sum::<usize>(),
            raw.len(),
            "splitting must lose no bytes — truncating would"
        );
    }

    /// A `\r`-only progress meter is the case the cap is for.
    #[test]
    fn a_carriage_return_only_meter_is_bounded() {
        let mut raw = Vec::new();
        for i in 0..20_000 {
            raw.extend_from_slice(format!("progress {i}\r").as_bytes());
        }
        let lines = feed(&[&raw]);
        assert!(
            lines.iter().all(|l| l.len() <= MAX_LINE_BYTES),
            "no row may exceed the cap"
        );
        assert_eq!(
            lines.iter().map(String::len).sum::<usize>(),
            raw.len() - 1,
            "every byte of the meter is still stored, just across rows — less the one \
             trailing `\\r`, which is a line ending rather than data"
        );
    }

    /// Multi-byte characters must not be corrupted by the split.
    #[test]
    fn the_cap_splits_on_a_character_boundary() {
        // 'é' is 2 bytes, so a naive cut at MAX_LINE_BYTES lands mid-character.
        let raw = "é".repeat(MAX_LINE_BYTES).into_bytes();
        let lines = feed(&[&raw]);
        assert!(
            !lines.iter().any(|l| l.contains('\u{fffd}')),
            "a split on a character boundary produces no replacement characters"
        );
        assert_eq!(
            lines.iter().map(|l| l.chars().count()).sum::<usize>(),
            MAX_LINE_BYTES,
            "every character survives"
        );
    }

    /// Chunk boundaries are not line boundaries — a line split across two reads,
    /// and a `\r\n` split between them, must still come out whole.
    #[test]
    fn a_line_split_across_chunks_is_reassembled() {
        assert_eq!(feed(&[b"he", b"ll", b"o\n"]), vec!["hello"]);
        assert_eq!(
            feed(&[b"crlf\r", b"\nnext\n"]),
            vec!["crlf", "next"],
            "a CRLF straddling two reads still ends exactly one line"
        );
    }
}
