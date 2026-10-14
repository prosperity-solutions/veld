//! The detached-node log sink: stdin to the central database.
//!
//! A node spawned by `process::spawn_detached` runs as
//! `sh -c '{ cmd; } 2>&1 | <own-exe> --log-sink …'`; this module is what that
//! `--log-sink` mode runs (`veld-daemon`'s `main` dispatches to [`run_log_sink`]
//! before it builds a runtime or initialises tracing). It was `veld _log` in the
//! CLI before node spawning moved into the daemon.

use std::path::PathBuf;

use super::log_budget;

/// What identifies the run and node whose output the sink stores.
///
/// Mirrors the frozen `--log-sink` argv: `--project-root`, `--run`, `--run-id`
/// (optional: pipelines started by a pre-v3 veld omit it), `--node`, `--variant`.
#[derive(Debug, Clone)]
pub struct LogSinkArgs {
    pub project_root: PathBuf,
    pub run: String,
    pub run_id: Option<String>,
    pub node: String,
    pub variant: String,
}

/// Read stdin to EOF, timestamp each line and store it in the central database.
/// Returns the process exit code (always 0: nothing here is fatal, see below).
pub fn run_log_sink(args: LogSinkArgs) -> i32 {
    let LogSinkArgs {
        project_root,
        run,
        run_id,
        node,
        variant,
    } = args;
    // Fast path: no config loading, no network — stdin → database.
    // Used internally by detached server mode; this process outlives
    // the CLI and keeps writing as long as the server produces output.
    //
    // This process is the read end of the server's stdout pipe: if it
    // ever exits while the server is running, the server takes
    // SIGPIPE on its next write and dies. So NOTHING here is fatal —
    // if the database can't be opened (downgrade, transient lock) we
    // keep draining stdin and drop the lines rather than kill the
    // environment.
    use std::io::BufRead;
    let db = crate::db::Db::open()
        .map_err(|e| {
            eprintln!("veld-daemon --log-sink: failed to open database, dropping logs: {e}")
        })
        .ok();

    // The database write happens on its own thread, fed over a channel,
    // for two reasons that both matter more here than anywhere else in
    // the tree.
    //
    // **Batching.** This is the highest-volume writer in the system —
    // 242,729 of one incident run's 342,328 log rows came through it,
    // one autocommit `INSERT` each. Batching is worth 9.8× in WAL pages
    // per line (see `Db::append_logs`), and a batch needs a flush
    // deadline, which a thread blocked in `read_line` cannot honour.
    //
    // **Not blocking the drain.** Read the comment above again: this
    // process is the read end of the server's stdout pipe, so if it
    // stops draining, the server blocks on a full pipe. Today the
    // database write is *on* the read path — a contended write waits up
    // to the 10 s `busy_timeout` with the pipe backing up behind it.
    // Moving the write off this thread means a slow database costs
    // buffered lines (bounded, counted, and reported below) instead of
    // stalling the environment.
    let writer = db.map(|db| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<(chrono::DateTime<chrono::Utc>, String)>(
            LOG_PUMP_QUEUE,
        );
        let scope = (
            project_root.clone(),
            run.clone(),
            run_id.clone(),
            node.clone(),
            variant.clone(),
        );
        // Shared, not passed through the channel, and owned by the
        // *writer* for reporting: a notice sent through the queue can
        // only be delivered when the queue has room, i.e. never in the
        // one situation it exists to describe. Measured before this:
        // 20,000 lines piped in, 2,592 dropped, and not one notice.
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let writer_dropped = std::sync::Arc::clone(&dropped);
        // Lines over the ingest budget (`log_budget`): counted and kept
        // separately from queue-full drops, because they are reported
        // differently and for a different reason.
        let shed = std::sync::Arc::new(log_budget::RateShed::default());
        let writer_shed = std::sync::Arc::clone(&shed);
        let handle = std::thread::spawn(move || {
            log_pump_writer(&db, &scope, &rx, &writer_dropped, &writer_shed);
        });
        (tx, handle, dropped, shed)
    });

    // **A stop must flush, and must not be slowed down.** Two wrong
    // answers were measured before this one.
    //
    // The *default* action kills this process instantly, and it is in the
    // process group `kill_process` signals (the detached pipeline is
    // `sh -c '{ cmd; } 2>&1 | veld-daemon --log-sink …'` spawned with
    // `process_group(0)`), so every `veld stop`, `veld restart` and
    // automatic recovery destroyed the in-flight batch and everything
    // queued — the last thing a dying server said, which is the output
    // most worth keeping.
    //
    // `SIG_IGN` fixes that and breaks something else. It leaves EOF as the
    // only way out, and EOF only arrives when every holder of the pipe's
    // write end is gone. A node that leaves a descendant outside the group
    // — `setsid`, or a self-daemonizing gradle/pm2/ngrok-shaped process —
    // keeps that write end open, so the pump waits, the group never
    // empties, and `kill_process` spends its whole 5-second budget before
    // SIGKILL. Measured: 5.0 s and the batch destroyed anyway, against
    // 0.17 s for the default action.
    //
    // Exiting *promptly* on the signal is the third wrong answer, and the
    // worst of them. This process is the read end of the server's stdout
    // pipe: leaving while the server is still writing SIGPIPEs it, killing
    // the thing mid-shutdown, and it also throws away the output a
    // graceful shutdown produces — which is the output somebody stopping
    // an environment most wants. Measured on the dev stack: a real
    // `veld stop` went from 0.49 s to 6.64 s and *lost* the daemon's
    // `shutdown signal received` / `veld-daemon stopped` lines.
    //
    // So: a real handler that records the signal, a read that is
    // *interrupted* by it (`sa_flags` deliberately omits `SA_RESTART`, so
    // `fill_buf` returns `ErrorKind::Interrupted`), and then **keep
    // draining under a deadline**. EOF still ends the loop normally, which
    // is what happens on every well-behaved node within milliseconds; the
    // deadline only decides how long a node that leaves the pipe held open
    // gets before this process flushes and leaves anyway. It sits inside
    // `kill_process`'s 5-second budget on purpose, so the pathological node
    // costs less than it did under `SIG_IGN` *and* gets its batch written
    // rather than being SIGKILLed mid-write.
    #[cfg(unix)]
    unsafe {
        extern "C" fn note_signal(_: libc::c_int) {
            // Async-signal-safe: one relaxed atomic store, nothing else.
            LOG_PUMP_TERMINATING.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = note_signal as *const () as libc::sighandler_t;
        action.sa_flags = 0; // no SA_RESTART: the read must see EINTR
        libc::sigemptyset(&mut action.sa_mask);
        // SAFETY: `note_signal` touches only an `AtomicBool`, so it is
        // legal to run in a signal handler.
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    // **The same line assembler the two foreground readers use.** It was
    // an inline copy of this loop here first; sharing it is what keeps the
    // detached path and the foreground ones from drifting on what a line
    // is, and `MAX_LINE_BYTES` is enforced while accumulating rather than
    // after `read_line` has already returned an unbounded one.
    let mut assembler = crate::process::LineAssembler::new();
    let mut lines: Vec<String> = Vec::new();

    // Hand completed lines to the writer thread.
    //
    // `try_send`, never `send`: blocking here puts the database back on
    // the path that drains the server's stdout pipe, and a reader that
    // stops draining blocks the server on a full pipe. Dropping is the
    // sanctioned failure mode for this process, and the count is owned by
    // the writer — see `log_pump_writer`, which reports it, because a
    // reporter on *this* side cannot report through a queue that is full.
    //
    // **The ingest budget is spent first**, so a line the budget refuses
    // never takes a queue slot — a crash-looping worker must not fill the
    // queue and turn its excess into "the database was not keeping up".
    // See `log_budget` for what is kept of a refused burst.
    let mut gate = log_budget::LogGate::for_log_pump(std::time::Instant::now());
    let mut queue = |lines: &mut Vec<String>| {
        for line in lines.drain(..) {
            if let Some((ref tx, _, ref dropped, ref shed)) = writer {
                let now = std::time::Instant::now();
                let Some(line) = gate.offer(now, (chrono::Utc::now(), line), shed) else {
                    continue;
                };
                if tx.try_send(line).is_err() {
                    dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    };

    // When the stop signal arrived, or `None` if it has not.
    let mut terminating_since: Option<std::time::Instant> = None;

    loop {
        // **Arm the deadline here, not only where EINTR is caught.** A
        // signal only interrupts a read the thread is *already* blocked
        // in; one delivered at any other moment — during the database open
        // and writer spawn above, or in the gap between finishing a chunk
        // and the next `fill_buf` — sets the flag and interrupts nothing,
        // so a version that armed only in the `Interrupted` arm below fell
        // straight back to blocking forever on a pipe a detached
        // descendant is holding open. That is precisely the failure this
        // whole mechanism replaced. Reproduced with a standalone harness:
        // the flag set outside a syscall left the next blocking read
        // unresponsive indefinitely.
        if terminating_since.is_none()
            && LOG_PUMP_TERMINATING.load(std::sync::atomic::Ordering::Relaxed)
        {
            terminating_since = Some(std::time::Instant::now());
        }

        // Once stopping, wait for readability rather than blocking
        // indefinitely, so a pipe somebody else is holding open cannot
        // keep this process alive to its SIGKILL. `poll` rather than a
        // non-blocking fd: it leaves `fill_buf`'s semantics alone, and a
        // partial read is the one thing this loop must not have to handle.
        if let Some(since) = terminating_since {
            let left = LOG_PUMP_STOP_GRACE.saturating_sub(since.elapsed());
            if left.is_zero() {
                break;
            }
            #[cfg(unix)]
            {
                let mut pfd = libc::pollfd {
                    fd: 0,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one initialised `pollfd` describing this
                // process's own stdin.
                let ready = unsafe {
                    libc::poll(&mut pfd, 1, left.as_millis().min(i32::MAX as u128) as i32)
                };
                // 0 is the deadline. A negative return is EINTR or a bad
                // fd, and it must go back to the top rather than fall
                // through — falling through reaches the blocking
                // `fill_buf` below *in the same iteration*, with the
                // signal already delivered and nothing left to interrupt
                // it, which is the pre-fix "wait for EOF however long that
                // takes" behaviour with extra steps.
                if ready == 0 {
                    break;
                }
                if ready < 0 {
                    continue;
                }
            }
        }

        let chunk = match reader.fill_buf() {
            Ok([]) => break, // EOF
            // A plain EINTR resumes: treating it as EOF would stop
            // draining while the server is still writing, and the server
            // would then block forever on a full pipe (same reasoning as
            // `drain_pipe`'s). A termination signal is picked up by the
            // check at the top of the loop, which is where it has to be —
            // see there.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
            Ok(buf) => buf.to_vec(),
        };
        reader.consume(chunk.len());

        assembler.push_chunk(&chunk, &mut lines);
        queue(&mut lines);
    }

    // Output that never got its newline is still output.
    if let Some(last) = assembler.finish() {
        lines.push(last);
        queue(&mut lines);
    }

    // Drop the sender so the writer sees the channel close, flushes its
    // partial batch and exits; then wait for it, so the last lines the
    // server produced are on disk before this process is.
    if let Some((tx, handle, _, _)) = writer {
        drop(tx);
        let _ = handle.join();
    }
    0
}

/// How long `veld-daemon --log-sink` keeps draining after a stop signal before it gives up.
///
/// Bounded well inside `kill_process`'s 5-second SIGTERM budget so this process
/// is never the reason a stop reaches SIGKILL, and long enough for a server's own
/// graceful shutdown output — the lines somebody stopping an environment most
/// wants — to arrive and be written. A well-behaved node's stdout closes within
/// milliseconds and never reaches this at all.
const LOG_PUMP_STOP_GRACE: std::time::Duration = std::time::Duration::from_millis(2_500);

/// Set by `veld-daemon --log-sink`'s SIGTERM/SIGINT handler; read by its reader loop.
///
/// A `static` rather than a captured variable because a signal handler is an
/// `extern "C" fn` and can capture nothing.
static LOG_PUMP_TERMINATING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Lines the `veld-daemon --log-sink` pump may have in flight to its writer thread.
///
/// A bound, not a buffer target: with batching the writer keeps up with anything
/// a real server produces, so this only fills when the database is genuinely
/// stuck.
///
/// **How much backlog, not how much time.** Two earlier versions of this comment
/// tried to express it as a duration and got the arithmetic wrong both times, so:
/// at the volume measured in the #332 incident, 16,384 lines is ~67 minutes of
/// output at the 4.06 rows/s mean and ~16 minutes at the 17.5 rows/s peak hour.
/// Neither figure is the useful one, because the queue only fills while the
/// *writer* is stalled — so what this bounds is how much history survives a
/// database that has stopped answering, and after that lines are dropped and
/// reported.
///
/// **Memory ceiling: `LOG_PUMP_QUEUE × MAX_LINE_BYTES × 3` = 3 GiB**, in the
/// pathological case of a process emitting nothing but maximal lines while the
/// database is wedged; a few MB for anything realistic.
///
/// The ×3 is not padding. Lines are queued as `String`s built with
/// `String::from_utf8_lossy`, which replaces every invalid byte with a 3-byte
/// U+FFFD — so 64 KiB of `0xFF` becomes 192 KiB, on exactly the input class this
/// subcommand documents itself as tolerating ("binary output from misbehaving
/// processes"). `the_queue_is_bounded_in_bytes_not_just_in_lines` asserts the
/// ceiling against a lossy-converted maximal line rather than against the product
/// of the two constants, because the product is the number that looks right and
/// is three times too small.
///
/// A *bounded* channel because the alternative fails worse: unbounded, a wedged
/// database turns a server's log volume into unbounded memory in a process the
/// user cannot see and did not start.
const LOG_PUMP_QUEUE: usize = 16_384;

/// How often the `veld-daemon --log-sink` writer looks at the ingest budget's bookkeeping while no
/// line is arriving.
const LOG_SHED_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The `veld-daemon --log-sink` pump's database side: batch what the reader thread sends and
/// write each batch as one transaction.
///
/// Exits when the channel closes (the reader hit EOF), after writing whatever is
/// buffered and reporting anything the reader had to drop.
fn log_pump_writer(
    db: &crate::db::Db,
    scope: &(std::path::PathBuf, String, Option<String>, String, String),
    rx: &std::sync::mpsc::Receiver<(chrono::DateTime<chrono::Utc>, String)>,
    dropped: &std::sync::atomic::AtomicU64,
    shed: &log_budget::RateShed,
) {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::RecvTimeoutError;

    let (project_root, run, run_id, node, variant) = scope;
    let mut batch: Vec<(chrono::DateTime<chrono::Utc>, String)> = Vec::new();

    let write = |batch: &mut Vec<(chrono::DateTime<chrono::Utc>, String)>| {
        if batch.is_empty() {
            return 0u64;
        }
        let n = batch.len() as u64;
        let lost = match db.append_logs(
            project_root,
            run,
            run_id.as_deref(),
            Some(node),
            Some(variant),
            crate::db::LogStream::Server,
            batch,
        ) {
            Ok(()) => 0,
            // A failed batch loses every line in it, where the pre-batching
            // shape lost one. Counted into the same total rather than discarded,
            // so the gap is reported rather than merely being absent.
            Err(_) => n,
        };
        batch.clear();
        lost
    };

    // Say what was lost, and say it on the **run-level `internal` stream**, not
    // on the node's own `server` stream.
    //
    // Two reasons, both load-bearing. It is written by veld, so it belongs on
    // veld's stream — and the supervised process can write anything it likes to
    // its own stream, including a forged `[VELD] dropped …` line. A reader
    // chasing lost logs must be able to tell veld's claim from the claim of the
    // program veld is watching, and the `internal` stream is the boundary that
    // makes that possible: nothing the child prints can reach it.
    // Returns whether the notice was written. **The caller must put the count
    // back on failure**: lines get dropped precisely *because* the database is
    // not keeping up, which is the same condition that makes this write fail — so
    // consuming the count on a failed write re-creates the silence the whole drop
    // accounting exists to remove, and does it in exactly the case that matters.
    let report_drops = |lost: u64| -> bool {
        if lost == 0 {
            return true;
        }
        db.append_log(
            project_root,
            run,
            run_id.as_deref(),
            None,
            None,
            crate::db::LogStream::Internal,
            chrono::Utc::now(),
            &format!(
                "[log] dropped {lost} line(s) from {node}:{variant} — the database was not \
                 keeping up with the process's output"
            ),
        )
        .is_ok()
    };

    // What the ingest budget refused (`log_budget`), as opposed to what the queue
    // refused. Called between batches and on the way out; `finishing` releases the
    // kept tail even though the burst has not been quiet for long, because there is
    // no later pass. `drained` says the queue was seen empty, so no line admitted
    // before the burst is still waiting to be written after its tail.
    //
    // Order is the point: the notice goes in first, on the `internal` stream, and the
    // kept tail follows as ordinary lines with their original timestamps and stream.
    // A failed notice hands the claim back untouched, and the tail is not written
    // without its notice.
    let report_shed = |finishing: bool, drained: bool| {
        let Some(claim) = shed.claim(std::time::Instant::now(), finishing, drained) else {
            return;
        };
        let kept = claim.tail.as_ref().map_or(0, Vec::len);
        let limit = format!(
            "output over the {} lines/s ingest limit ({} line burst)",
            log_budget::LOG_RATE_PER_SEC,
            log_budget::LOG_BURST
        );
        let message = match (claim.dropped, kept) {
            (0, k) => format!(
                "[log] rate limit: re-adding the last {k} dropped line(s) from {node}:{variant} \
                 at their original timestamps"
            ),
            (n, 0) => {
                format!("[log] rate limit: dropped {n} line(s) from {node}:{variant} — {limit}")
            }
            (n, k) => format!(
                "[log] rate limit: dropped {n} line(s) from {node}:{variant} — {limit}; the \
                 last {k} dropped line(s) are re-added at their original timestamps"
            ),
        };
        let written = db
            .append_log(
                project_root,
                run,
                run_id.as_deref(),
                None,
                None,
                crate::db::LogStream::Internal,
                chrono::Utc::now(),
                &message,
            )
            .is_ok();
        if !written {
            shed.restore(claim);
            return;
        }
        if let Some(mut tail) = claim.tail {
            let lost = write(&mut tail);
            dropped.fetch_add(lost, Ordering::Relaxed);
        }
    };

    // Nothing buffered: block for the first line rather than waking every flush
    // interval on an idle server — except while the budget has refused lines, when
    // a short timeout lets a process that went quiet get its notice and its tail
    // without waiting for another line (or for EOF) that may never come. `recv`
    // erroring means the reader thread hit EOF and dropped the sender.
    loop {
        let first = if shed.pending() {
            match rx.recv_timeout(LOG_SHED_POLL) {
                Ok(item) => item,
                Err(RecvTimeoutError::Timeout) => {
                    report_shed(false, true);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(item) => item,
                Err(_) => break,
            }
        };
        batch.push(first);

        // The deadline belongs to the batch, not to each line — see
        // `crate::logging::LogBatch`, which is the async half of this and
        // documents why a per-line deadline is the bug to avoid.
        let deadline = std::time::Instant::now() + crate::logging::LOG_BATCH_FLUSH;
        let mut closed = false;
        // Whether the batch ended because the queue ran dry, not because it was full
        // or out of time.
        let mut drained = false;
        while batch.len() < crate::logging::LOG_BATCH_MAX_LINES {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok(item) => batch.push(item),
                Err(RecvTimeoutError::Timeout) => {
                    drained = true;
                    break;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    closed = true;
                    drained = true;
                    break;
                }
            }
        }
        let lost = write(&mut batch);
        dropped.fetch_add(lost, Ordering::Relaxed);

        // Report between batches, so a run that drops lines in the middle says
        // so while it is still running rather than only at the end. `swap` claims
        // the count exactly once; a failed write hands it straight back, so the
        // next batch retries rather than losing it.
        let claimed = dropped.swap(0, Ordering::Relaxed);
        if !report_drops(claimed) {
            dropped.fetch_add(claimed, Ordering::Relaxed);
        }
        report_shed(false, drained);

        if closed {
            break;
        }
    }

    let lost = write(&mut batch);
    dropped.fetch_add(lost, Ordering::Relaxed);
    // The residual, on the way out. This is the case that was silent: drops at the
    // tail of a run, or a queue that stayed full to the end, left no trace
    // anywhere at all. There is no later pass to retry from, so if this write
    // fails too the count really is lost — say so rather than implying otherwise.
    //
    // The `eprintln!` is **not** a fallback for the detached case, and should not
    // be read as one: `spawn_detached` gives the `sh` wrapper
    // `.stderr(Stdio::null())`, and the script's `2>&1` redirects only the `{ … }`
    // compound, so this process inherits fd 2 = `/dev/null` in every production
    // run. It is here for somebody running `veld-daemon --log-sink` by hand while debugging
    // exactly this, and for nothing else.
    let claimed = dropped.swap(0, Ordering::Relaxed);
    if !report_drops(claimed) && claimed > 0 {
        eprintln!(
            "veld-daemon --log-sink: dropped {claimed} line(s) from {node}:{variant} and could not record it"
        );
    }
    report_shed(true, true);
}

#[cfg(test)]
mod log_pump_tests {
    use super::super::log_budget::{self, RateShed};
    use super::{LOG_PUMP_QUEUE, log_pump_writer};
    use crate::db::{Db, LogFilter, LogStream};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    type Scope = (std::path::PathBuf, String, Option<String>, String, String);

    fn scope() -> Scope {
        (
            std::path::PathBuf::from("/proj"),
            "dev".to_owned(),
            Some("rid".to_owned()),
            "api".to_owned(),
            "local".to_owned(),
        )
    }

    fn stored(db: &Db, stream: LogStream) -> Vec<String> {
        db.tail_logs(
            std::path::Path::new("/proj"),
            "dev",
            &LogFilter {
                streams: Some(vec![stream.as_str()]),
                ..Default::default()
            },
            100_000,
        )
        .unwrap()
        .into_iter()
        .map(|r| r.line)
        .collect()
    }

    /// The one derived number in `LOG_PUMP_QUEUE`'s doc comment, asserted rather
    /// than restated.
    ///
    /// That comment has been wrong four times across this branch's review rounds —
    /// on the duration, twice, and on the memory ceiling, twice — and the ceiling
    /// is the claim that matters, because it is what makes a bounded channel a
    /// bound at all. It holds only while the reader enforces the per-line cap, so
    /// changing either constant should force somebody to re-read the prose.
    #[test]
    fn the_queue_is_bounded_in_bytes_not_just_in_lines() {
        // A maximal line of bytes that are not valid UTF-8, converted the way the
        // reader converts one. `from_utf8_lossy` turns each `0xFF` into a 3-byte
        // U+FFFD, so this is 3x the raw cap — the factor the product of the two
        // constants silently omits.
        let worst_line =
            String::from_utf8_lossy(&vec![0xFFu8; crate::process::MAX_LINE_BYTES]).into_owned();
        assert_eq!(
            worst_line.len(),
            3 * crate::process::MAX_LINE_BYTES,
            "lossy conversion is where the factor of three comes from; if this changes, the \
             ceiling below and `LOG_PUMP_QUEUE`'s doc comment both need revisiting"
        );

        let ceiling = LOG_PUMP_QUEUE as u64 * worst_line.len() as u64;
        assert_eq!(
            ceiling,
            3 * 1024 * 1024 * 1024,
            "this is the worst-case resident size of a `veld-daemon --log-sink` process, and \
             `LOG_PUMP_QUEUE`'s doc comment states it in prose. If you changed either \
             constant, update that comment too."
        );
    }

    /// Everything the reader sends is written, in order, and closing the sender
    /// mid-batch still flushes what was buffered.
    ///
    /// 1,200 lines crosses `LOG_BATCH_MAX_LINES` (512) twice, so two batches
    /// flush on the size cap and the remaining 176 can only reach the database
    /// through the post-loop flush.
    #[test]
    fn every_line_reaches_the_database_including_the_final_partial_batch() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("pump.db")).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(LOG_PUMP_QUEUE);
        for i in 0..1_200 {
            tx.send((chrono::Utc::now(), format!("line {i}"))).unwrap();
        }
        drop(tx);

        let dropped = AtomicU64::new(0);
        log_pump_writer(&db, &scope(), &rx, &dropped, &RateShed::default());

        let rows = stored(&db, LogStream::Server);
        assert_eq!(
            rows.len(),
            1_200,
            "no line may be lost at the batch boundary"
        );
        assert_eq!(rows[0], "line 0");
        assert_eq!(rows[1_199], "line 1199");
    }

    /// Lines the reader had to drop are reported, and reported on the run-level
    /// `internal` stream rather than on the node's own.
    ///
    /// **This is the case that was silent.** The count used to be reported by
    /// piggy-backing on the next line that fit in the queue, so a run that
    /// dropped lines at its tail — or one whose queue stayed full to the end —
    /// left no trace anywhere. Measured before the fix: 20,000 lines piped into
    /// `veld-daemon --log-sink`, 2,592 dropped, and not one notice written.
    ///
    /// The stream matters as much as the count: the supervised process can print
    /// anything it likes onto its own `server` stream, including a forged notice.
    /// `internal` is the boundary that keeps veld's claim distinguishable from
    /// the claim of the program veld is watching.
    #[test]
    fn dropped_lines_are_reported_on_the_internal_stream() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("pump.db")).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(LOG_PUMP_QUEUE);
        tx.send((chrono::Utc::now(), "survivor".to_owned()))
            .unwrap();
        drop(tx);

        // What the reader does when `try_send` is refused.
        let dropped = AtomicU64::new(7);
        log_pump_writer(&db, &scope(), &rx, &dropped, &RateShed::default());

        assert_eq!(stored(&db, LogStream::Server), vec!["survivor".to_owned()]);

        let internal = stored(&db, LogStream::Internal);
        assert_eq!(internal.len(), 1, "exactly one notice: {internal:?}");
        assert!(
            internal[0].contains("dropped 7 line(s)") && internal[0].contains("api:local"),
            "the notice must name the count and the node it belongs to: {internal:?}"
        );
    }

    /// A run that dropped nothing says nothing — the notice is a real signal, so
    /// it must not appear on every healthy run.
    #[test]
    fn a_run_that_dropped_nothing_writes_no_notice() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("pump.db")).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(LOG_PUMP_QUEUE);
        tx.send((chrono::Utc::now(), "only line".to_owned()))
            .unwrap();
        drop(tx);

        let dropped = AtomicU64::new(0);
        log_pump_writer(&db, &scope(), &rx, &dropped, &RateShed::default());

        assert!(stored(&db, LogStream::Internal).is_empty());
    }

    /// The count is claimed exactly once, so a long run does not repeat the same
    /// notice on every batch afterwards.
    #[test]
    fn a_drop_count_is_reported_once_and_then_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("pump.db")).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(LOG_PUMP_QUEUE);
        // Two batches' worth, so the writer reports between them and again on
        // the way out.
        for i in 0..(crate::logging::LOG_BATCH_MAX_LINES + 10) {
            tx.send((chrono::Utc::now(), format!("l{i}"))).unwrap();
        }
        drop(tx);

        let dropped = Arc::new(AtomicU64::new(3));
        log_pump_writer(&db, &scope(), &rx, &dropped, &RateShed::default());

        let internal = stored(&db, LogStream::Internal);
        assert_eq!(
            internal.len(),
            1,
            "the same drop count must not be re-reported on every later batch: {internal:?}"
        );
        assert_eq!(dropped.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    /// Lines the ingest budget refused are reported as a *rate limit* on the
    /// `internal` stream, and the end of the burst is written back after the notice.
    ///
    /// The notice must be distinguishable from the queue-full one (a reader chasing
    /// lost logs needs to know whether the database or the process was the cause), and
    /// the kept tail must be ordinary `server` lines with their original timestamps —
    /// it is the fatal error at the end of a crash loop that `veld start` and the
    /// orchestrator show the user.
    #[test]
    fn rate_limited_lines_are_reported_on_internal_and_the_tail_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("pump.db")).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(LOG_PUMP_QUEUE);
        tx.send((chrono::Utc::now(), "stored".to_owned())).unwrap();
        drop(tx);

        let shed = RateShed::default();
        let t0 = std::time::Instant::now();
        let original = chrono::Utc::now() - chrono::Duration::hours(1);
        for i in 0..80 {
            shed.record(t0, (original, format!("shed {i}")));
        }
        log_pump_writer(&db, &scope(), &rx, &AtomicU64::new(0), &shed);

        let internal = stored(&db, LogStream::Internal);
        assert_eq!(internal.len(), 1, "exactly one notice: {internal:?}");
        assert!(
            internal[0].contains("rate limit")
                && internal[0].contains("dropped 80 line(s)")
                && internal[0].contains("api:local")
                && internal[0].contains("re-added"),
            "the notice must name the rate limit, the count and the node: {internal:?}"
        );
        assert!(
            !internal[0].contains("database was not keeping up"),
            "a rate-limit notice must not read as a queue-full one: {internal:?}"
        );

        let server = stored(&db, LogStream::Server);
        assert_eq!(server.len(), 1 + log_budget::SHED_TAIL_LINES);
        assert_eq!(server[0], "stored");
        // Only the newest 50 of the 80 survive, in order.
        assert_eq!(server[1], "shed 30");
        assert_eq!(server[50], "shed 79");
        let rows = db
            .tail_logs(
                std::path::Path::new("/proj"),
                "dev",
                &LogFilter {
                    streams: Some(vec![LogStream::Server.as_str()]),
                    ..Default::default()
                },
                100,
            )
            .unwrap();
        assert!(
            rows.iter().skip(1).all(|r| {
                chrono::DateTime::parse_from_rfc3339(&r.ts)
                    .is_ok_and(|t| t.timestamp() == original.timestamp())
            }),
            "the kept lines carry the time they were printed, not the time they were written"
        );
        assert!(!shed.pending());
    }

    /// A process that goes quiet after being rate-limited gets its notice and tail
    /// without another line or EOF arriving: the writer polls while a burst is pending.
    #[test]
    fn a_quiet_process_still_gets_its_tail_written() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("pump.db")).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(LOG_PUMP_QUEUE);
        let shed = Arc::new(RateShed::default());
        // Shed "a long time ago" so the quiet window has already passed.
        let long_ago = std::time::Instant::now() - log_budget::SHED_QUIET * 2;
        shed.record(long_ago, (chrono::Utc::now(), "fatal: boom".to_owned()));

        let writer_shed = Arc::clone(&shed);
        let handle = std::thread::scope(|scope_| {
            let (db, writer_shed) = (&db, &writer_shed);
            let h = scope_.spawn(move || {
                log_pump_writer(db, &scope(), &rx, &AtomicU64::new(0), writer_shed);
            });
            // The sender stays open: this is a live process that has gone quiet.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while shed.pending() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let flushed = !shed.pending();
            drop(tx);
            h.join().unwrap();
            flushed
        });
        assert!(
            handle,
            "the tail must be flushed while the sender is still open"
        );
        assert_eq!(
            stored(&db, LogStream::Server),
            vec!["fatal: boom".to_owned()]
        );
        assert_eq!(stored(&db, LogStream::Internal).len(), 1);
    }

    /// A run that was never rate-limited writes no rate-limit notice.
    #[test]
    fn an_unthrottled_run_writes_no_rate_limit_notice() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_at(&dir.path().join("pump.db")).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(LOG_PUMP_QUEUE);
        tx.send((chrono::Utc::now(), "fine".to_owned())).unwrap();
        drop(tx);
        log_pump_writer(&db, &scope(), &rx, &AtomicU64::new(0), &RateShed::default());
        assert!(stored(&db, LogStream::Internal).is_empty());
    }
}
