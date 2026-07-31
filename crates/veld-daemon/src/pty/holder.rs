//! The holder process: one per terminal session, and the reason a shell
//! survives the daemon.
//!
//! # Why this process exists
//!
//! A PTY has two ends. The shell holds the slave; whoever holds the **master**
//! can read what the shell prints and write its input. When the last holder of
//! the master goes away the kernel hangs up the shell's foreground process
//! group, and a shell told its terminal was unplugged exits, taking whatever ran
//! inside it. While the daemon held the master, `veld update` — which restarts
//! the daemon — therefore ended every terminal, and no amount of care on the
//! daemon's shutdown path could change that: the descriptor dies with the
//! process. `pty::shutdown_sessions` used to hang the shells up *deliberately*,
//! because the alternative was the same death half a second later with orphaned
//! grandchildren.
//!
//! So the master moved here. This process owns the PTY, the shell, the
//! [`Scrollback`](super::Scrollback) ring and the exit code, and serves them over
//! a unix socket that a daemon connects to. Restart the daemon and the shell
//! never notices; the new daemon finds the socket, handshakes, and picks up
//! where its predecessor left off.
//!
//! # One holder per session, deliberately
//!
//! A single supervisor owning every PTY would recreate the original problem one
//! level down — it would itself need updating, and the handover of a global
//! registry is the hard part of that. Per-session, there is nothing global to
//! hand over: a new daemon spawns new holders from the new binary while old
//! holders keep serving old sessions until their shells end. It also isolates
//! failure, since one holder lost is one terminal lost.
//!
//! # What must not regress
//!
//! - **The daemon never signals the shell.** This process owns the unreaped
//!   child, so it is the only one that can signal the process group without
//!   racing `wait()` — the moment a child is reaped its pid is reusable, and a
//!   late `killpg` would then hit an unrelated group. The daemon asks, via
//!   [`wire::HANGUP`].
//! - **Nothing here is a security boundary.** The socket is `0600` inside a
//!   `0700` directory, so reaching it already means being the user — the same
//!   trust level as the daemon's own SQLite database. The `Origin`/ticket gates
//!   that authorise a *browser* stay where they are, in front of the WebSocket.
//! - **The holder must outlive its parent's process group.** It is spawned with
//!   `process_group(0)` for exactly the reason Caddy is (see
//!   `veld_core::setup`): otherwise launchd's `bootout` and systemd's default
//!   `KillMode=control-group` take it down with the daemon, which is the failure
//!   this whole module exists to prevent.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use anyhow::Context;
use portable_pty::{CommandBuilder, MasterPty, PtyPair, PtySize, native_pty_system};
use tokio::io::unix::AsyncFd;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::wire::{self, Frame, HolderConfig};
use super::{EXIT_DRAIN, KILL_GRACE, MAX_DIMENSION, READ_BUF, Scrollback, clamp_dimension};

/// Buffered output frames per connection. Matches the daemon's own output
/// channel: past this the socket is not keeping up, and the *daemon* is the side
/// that reports a lagging client.
const OUT_CHANNEL: usize = 512;

/// Commands buffered from the connection reader and the acceptor.
const CMD_CHANNEL: usize = 64;

/// How long a peer gets to accept the greeting before the connection is
/// abandoned.
///
/// Separate from [`OUTPUT_SEND_TIMEOUT`] because it bounds a different thing: the
/// greeting is written from the main loop, so a peer that connects and never reads
/// parks everything else this holder has to do.
const GREETING_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a connected daemon gets to accept one output frame before the
/// connection is treated as dead.
///
/// Generous — the peer is a local process whose only job on this socket is to
/// drain it — because the cost of being wrong is a dropped connection, and the
/// cost of having no bound at all is a holder that cannot be hung up.
const OUTPUT_SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the holder waits, after handing a daemon the exit code, for that
/// daemon to close the connection.
///
/// The daemon closes as soon as it has recorded the exit, so this is only a
/// bound on a peer that does not — a different protocol version, or a bug.
/// Nothing is lost by giving up: the daemon already has the code and the
/// scrollback, and keeps them for its own detach grace exactly as it did when it
/// owned the PTY itself.
const EXIT_LINGER: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Read the configuration from stdin and serve one session until it ends.
///
/// Called from `main` for `veld-daemon --pty-holder`. Reading the config from
/// stdin rather than `argv` keeps paths out of the process table, and closing
/// stdin is the parent's signal that it has finished writing.
pub async fn run_from_stdin() -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut raw = String::new();
    tokio::io::stdin()
        .read_to_string(&mut raw)
        .await
        .context("failed to read the holder configuration from stdin")?;
    let cfg: HolderConfig =
        serde_json::from_str(raw.trim()).context("invalid holder configuration on stdin")?;
    run(cfg).await
}

/// Serve one session: own its PTY, accept daemons, end when the shell does.
pub async fn run(cfg: HolderConfig) -> anyhow::Result<()> {
    let listener = bind(&cfg.socket)?;
    // The socket is the daemon's only way in, so a failure past this point must
    // still remove it — otherwise the next daemon boot finds a door with nobody
    // behind it and has to time out to learn that.
    let result = serve(&cfg, listener).await;
    let _ = std::fs::remove_file(&cfg.socket);
    result
}

/// Bind the holder's socket, replacing a stale file but never a live one.
///
/// The probe mirrors the daemon's own startup: a leftover socket *file* from a
/// killed holder must not stop this one, but a socket somebody answers means a
/// holder for this session is already running and this process must not fight it
/// for the shell.
fn bind(path: &std::path::Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create the holder socket directory")?;
        // 0700, and a failure here is fatal rather than ignored: the **directory**
        // is what keeps another local account away from a socket that hands out a
        // shell. `bind` below creates the socket itself at the process umask
        // (0755 with the usual one) and only then chmods it, so for a moment the
        // socket's own mode is not restrictive — which is fine inside a 0700
        // directory and not fine outside one. If this cannot be enforced (a
        // foreign-owned `VELD_PTY_DIR`, say), refuse to serve rather than serving
        // from an open directory.
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("could not restrict {}", parent.display()))?;
    }
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => anyhow::bail!(
                    "a holder for this session is already listening on {}",
                    path.display()
                ),
                Err(_) => {
                    std::fs::remove_file(path).context("failed to remove a stale holder socket")?;
                    UnixListener::bind(path).context("failed to bind the holder socket")?
                }
            }
        }
        Err(e) => return Err(e).context("failed to bind the holder socket"),
    };
    // 0600 on the socket too. Defence in depth only — see the directory above for
    // why the directory is the real gate.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .context("failed to restrict the holder socket")?;
    Ok(listener)
}

// ---------------------------------------------------------------------------
// The loop
// ---------------------------------------------------------------------------

/// What the main loop reacts to besides its PTY.
enum Cmd {
    /// A daemon connected. It displaces whatever was connected before, the same
    /// way a second WebSocket takes over from the first.
    Connected(UnixStream),
    /// A frame from the connection of the given generation.
    Frame(u64, Frame),
    /// That generation's reader ended.
    Disconnected(u64),
}

/// The daemon-facing side of one connection.
struct Conn {
    generation: u64,
    out: mpsc::Sender<(u8, Vec<u8>)>,
}

async fn serve(cfg: &HolderConfig, listener: UnixListener) -> anyhow::Result<()> {
    let size = PtySize {
        cols: clamp_dimension(Some(cfg.cols), 80),
        rows: clamp_dimension(Some(cfg.rows), 24),
        pixel_width: 0,
        pixel_height: 0,
    };
    let Spawned {
        master,
        mut child,
        pid,
        read_fd,
        write_fd,
    } = spawn_shell(&cfg.cwd, size).context("failed to open a pty")?;
    info!(
        session = %cfg.session_id,
        worktree = %cfg.label,
        pid,
        "terminal holder started"
    );

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Cmd>(CMD_CHANNEL);
    tokio::spawn(acceptor(listener, cmd_tx.clone()));

    let mut scrollback = Scrollback::new();
    let mut conn: Option<Conn> = None;
    let mut generation = 0u64;
    let mut disconnected_since = Some(Instant::now());
    let mut exit_code: Option<u32> = None;
    let orphan_grace = Duration::from_secs(cfg.orphan_grace_secs);

    // `child.wait()` is blocking; on the pool it both reaps the zombie and
    // yields the status.
    let mut waiter = tokio::task::spawn_blocking(move || child.wait());
    let mut buf = [0u8; READ_BUF];
    // Placeholder deadline: the branch is disabled until the shell exits and the
    // timer is reset at that moment, so this duration never fires.
    let drain = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(drain);
    // Armed only while nothing is connected, using the same pattern as `drain`
    // above: an `interval` would wake every holder every 30 seconds for the whole
    // life of the terminal, including the normal case where a daemon is attached
    // and there is nothing to check — a dozen open panes then means a steady
    // trickle of pointless wakeups on a laptop.
    // Armed at boot because nothing is connected yet; re-armed on every
    // disconnect by `mark_disconnected!`.
    let orphan_deadline = tokio::time::sleep(orphan_grace);
    tokio::pin!(orphan_deadline);
    let mut asked_to_hang_up = false;
    // Set once the orphan path has done everything it can (hung up, then killed),
    // which disables its branch — see the guard on it below.
    let mut orphan_handled = false;

    /// Record that no daemon is connected, and re-arm the orphan deadline.
    ///
    /// The two must happen together. A `sleep` elapses on its own schedule
    /// whether or not its branch is being polled, so a holder that spent longer
    /// than the grace *with a daemon attached* would find the deadline already
    /// expired the instant it lost that daemon — and would hang up the shell
    /// immediately on the next `veld update`, which is exactly the failure this
    /// module exists to prevent. Every disconnect therefore resets it.
    macro_rules! mark_disconnected {
        () => {{
            if disconnected_since.is_none() {
                disconnected_since = Some(Instant::now());
            }
            orphan_deadline
                .as_mut()
                .reset(tokio::time::Instant::now() + orphan_grace);
            orphan_handled = false;
        }};
    }

    loop {
        // Deliberately NOT `biased`. With the PTY branch first, a shell
        // producing continuously (a build, a `yes`) keeps it permanently ready
        // and the hangup, connect and exit branches are never polled — the same
        // starvation the daemon's socket loop documents.
        tokio::select! {
            r = pty_read(&read_fd, &mut buf) => match r {
                // EOF: every descriptor on the slave side is closed.
                Ok(0) => break,
                Ok(n) => {
                    scrollback.push(&buf[..n]);
                    if let Some(c) = &conn {
                        // Awaited rather than `try_send`, because this hop is a
                        // local socket with a daemon that does nothing but drain
                        // it: dropping bytes on a momentarily full queue would
                        // corrupt the screen for no gain, and backpressure to the
                        // shell is what a real terminal does.
                        //
                        // Bounded, though. The await is inside this branch, so
                        // until it returns the loop is not polling the hangup, the
                        // orphan deadline or the exit — and a wedged daemon (this
                        // module has had one) would otherwise park the holder's
                        // whole control path indefinitely.
                        //
                        // A timeout **discards this chunk and keeps the
                        // connection**. Dropping the connection instead was the
                        // first version and it was worse than the problem: the
                        // daemon reads its end of this socket in a task that does
                        // nothing else, so a stall means a wedged runtime — and
                        // closing on it made the daemon declare the session exited
                        // (code 1) while the shell was alive and well in here. A
                        // discarded chunk is the *existing* lossy behaviour of the
                        // hop in front of it, which reports itself to the client as
                        // `lagged`; the bytes also stay in the scrollback, so the
                        // next attach replays them.
                        let queued = tokio::time::timeout(
                            OUTPUT_SEND_TIMEOUT,
                            c.out.send((wire::OUTPUT, buf[..n].to_vec())),
                        )
                        .await;
                        let dead = match queued {
                            Ok(Ok(())) => false,
                            // The writer task is gone; its reader reports the
                            // disconnect too, and this is idempotent.
                            Ok(Err(_)) => true,
                            Err(_) => {
                                warn!(
                                    session = %cfg.session_id,
                                    bytes = n,
                                    "daemon has not read for {}s — dropping this output",
                                    OUTPUT_SEND_TIMEOUT.as_secs()
                                );
                                false
                            }
                        };
                        if dead {
                            conn = None;
                            mark_disconnected!();
                        }
                    }
                }
                // Linux reports the same hangup as EIO rather than EOF.
                Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
                Err(e) => {
                    warn!("terminal read failed (pid {pid}): {e}");
                    break;
                }
            },

            Some(cmd) = cmd_rx.recv() => match cmd {
                Cmd::Connected(stream) => {
                    generation += 1;
                    conn = attach(
                        stream,
                        generation,
                        cmd_tx.clone(),
                        cfg,
                        pid,
                        exit_code,
                        &scrollback,
                        disconnected_since,
                    )
                    .await;
                    if conn.is_some() {
                        disconnected_since = None;
                    } else {
                        // The greeting failed, so nothing is attached after all.
                        mark_disconnected!();
                    }
                }
                Cmd::Frame(seq, frame) => {
                    // `HANGUP` is honoured whatever the generation, and even when
                    // no connection is established at all. It is the one frame
                    // whose entire purpose is to work in degraded conditions — a
                    // peer that could not be greeted, an unknown protocol version,
                    // `veld uninstall` — and gating it on being the current writer
                    // is what made those callers silently ineffective. It ends a
                    // session rather than writing to it, so the one-writer rule
                    // does not apply.
                    if frame.kind == wire::HANGUP {
                        info!(session = %cfg.session_id, "holder asked to hang up");
                        asked_to_hang_up = true;
                        if exit_code.is_none() {
                            hangup(pid);
                        }
                    }
                    // Everything else: a frame from a displaced connection is not
                    // acted on, because it would be a second writer on one input
                    // stream — the rule the whole session model rests on.
                    else if conn.as_ref().is_some_and(|c| c.generation == seq) {
                        match frame.kind {
                            wire::INPUT => {
                                if pty_write(&write_fd, &frame.payload).await.is_err() {
                                    debug!("terminal input write failed (pid {pid})");
                                }
                            }
                            wire::RESIZE => match wire::decode_size(&frame.payload) {
                                Some((cols, rows)) => resize(master.as_ref(), cols, rows),
                                None => warn!("ignoring a malformed resize frame"),
                            },
                            other if frame.is_ignorable() => {
                                debug!("ignoring holder-numbered frame {other:#x}");
                            }
                            other => {
                                warn!("dropping connection: unsupported frame {other:#x}");
                                conn = None;
                                mark_disconnected!();
                            }
                        }
                    }
                }
                Cmd::Disconnected(seq) => {
                    if conn.as_ref().is_some_and(|c| c.generation == seq) {
                        debug!(session = %cfg.session_id, "daemon disconnected");
                        conn = None;
                        mark_disconnected!();
                    }
                }
            },

            status = &mut waiter, if exit_code.is_none() => {
                exit_code = Some(match status {
                    Ok(Ok(s)) => s.exit_code(),
                    Ok(Err(e)) => {
                        warn!("waiting on terminal shell (pid {pid}) failed: {e}");
                        1
                    }
                    Err(e) => {
                        warn!("terminal wait task for pid {pid} failed: {e}");
                        1
                    }
                });
                // The shell is gone, but a grandchild it left behind can still
                // hold the slave open, in which case EOF never arrives. Bound
                // the wait for it.
                drain.as_mut().reset(tokio::time::Instant::now() + EXIT_DRAIN);
            },

            _ = &mut drain, if exit_code.is_some() => break,

            // Guarded on `!orphan_handled` as well as being disconnected: a
            // `Sleep` that has elapsed returns `Ready` on **every** subsequent
            // poll, so without this the branch is selected on every iteration of
            // the loop. A shell that ignores SIGHUP then pegged a core forever —
            // a busy spin introduced by the fix that made this a deadline instead
            // of an interval.
            _ = &mut orphan_deadline, if disconnected_since.is_some() && !orphan_handled => {
                // The leak bound: a daemon that never comes back (uninstalled,
                // crash-looping, the user logged out) must not leave a shell
                // running for the uptime of the box. `exit_code.is_none()` for
                // the same reason the hangup frame checks it — past that point
                // the pid may already have been reaped and reused.
                if exit_code.is_none() {
                    if !asked_to_hang_up {
                        info!(
                            session = %cfg.session_id,
                            "no daemon for {}s — hanging up the shell",
                            orphan_grace.as_secs()
                        );
                        asked_to_hang_up = true;
                        hangup(pid);
                        // Give SIGHUP its grace, then come back once to escalate.
                        // The loop only *exits* on the pty closing, so a shell
                        // that ignores the hangup and holds the pty open would
                        // otherwise never be collected at all.
                        orphan_deadline
                            .as_mut()
                            .reset(tokio::time::Instant::now() + KILL_GRACE);
                    } else {
                        warn!(
                            session = %cfg.session_id,
                            "shell ignored the orphan hangup — killing it"
                        );
                        kill(pid);
                        orphan_handled = true;
                    }
                } else {
                    orphan_handled = true;
                }
            },
        }
    }

    let code = match exit_code {
        Some(c) => c,
        None => {
            // The read loop ended without the shell exiting: it was closed out
            // from under us, or a descriptor error broke the loop. Hang up
            // (idempotent if a HANGUP already did) and escalate if it holds out.
            hangup(pid);
            match tokio::time::timeout(KILL_GRACE, &mut waiter).await {
                Ok(Ok(Ok(s))) => s.exit_code(),
                Ok(_) => 1,
                Err(_) => {
                    kill(pid);
                    let _ = waiter.await;
                    1
                }
            }
        }
    };

    // The notice goes to the scrollback *and* the live stream, before the exit
    // code: a scrollback-only notice is invisible to whoever is watching now,
    // and publishing the code first would race the notice past it.
    let notice = exit_notice(code);
    scrollback.push(&notice);
    debug!(session = %cfg.session_id, pid, code, "terminal shell exited");

    deliver_exit(
        cfg,
        pid,
        code,
        &notice,
        &scrollback,
        conn,
        generation,
        cmd_rx,
        cmd_tx,
        disconnected_since,
        // A shell that was hung up on purpose has nothing to keep: whoever asked
        // for it initiated the end and has already dropped the session, so there
        // is no future daemon that needs to be told. Lingering anyway left a
        // holder process — and its socket, and the session slot a later daemon
        // would spend adopting it — alive for the whole orphan grace after every
        // closed tab.
        if asked_to_hang_up {
            Duration::ZERO
        } else {
            orphan_grace
        },
    )
    .await;
    Ok(())
}

/// The line a client sees in place of a prompt once the shell is gone.
fn exit_notice(code: u32) -> Vec<u8> {
    format!("\r\n\x1b[2m[veld] shell exited ({code})\x1b[0m\r\n").into_bytes()
}

/// Hand the exit code to a daemon, then stop.
///
/// Once a daemon has the code it also has the scrollback, and it keeps both for
/// its own detach grace — which is exactly what happened when the daemon owned
/// the PTY, so a post-mortem survives a socket drop but not a daemon restart.
/// Matching that rather than lingering for 30 minutes per dead terminal is
/// deliberate.
#[allow(clippy::too_many_arguments)]
async fn deliver_exit(
    cfg: &HolderConfig,
    pid: i32,
    code: u32,
    notice: &[u8],
    scrollback: &Scrollback,
    mut conn: Option<Conn>,
    mut generation: u64,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    cmd_tx: mpsc::Sender<Cmd>,
    disconnected_since: Option<Instant>,
    // How long to wait for a daemon to hand the exit code to. Zero when the
    // shell was hung up deliberately.
    peer_grace: Duration,
) {
    let mut delivered = false;

    if let Some(c) = &conn {
        let sent = c.out.send((wire::OUTPUT, notice.to_vec())).await.is_ok()
            && c.out
                .send((wire::EXIT, code.to_be_bytes().to_vec()))
                .await
                .is_ok();
        delivered = sent;
        if !sent {
            conn = None;
        }
    }

    let wait_for_peer = if peer_grace.is_zero() {
        Duration::ZERO
    } else if delivered {
        EXIT_LINGER
    } else {
        // Nobody to tell yet: wait for a daemon, but no longer than the shell
        // itself would have been kept.
        let waited = disconnected_since
            .map(|s| Instant::now().saturating_duration_since(s))
            .unwrap_or_default();
        peer_grace.saturating_sub(waited)
    };

    let _ = tokio::time::timeout(wait_for_peer, async {
        loop {
            match cmd_rx.recv().await {
                Some(Cmd::Connected(stream)) => {
                    generation += 1;
                    if let Some(c) = attach(
                        stream,
                        generation,
                        cmd_tx.clone(),
                        cfg,
                        pid,
                        Some(code),
                        scrollback,
                        disconnected_since,
                    )
                    .await
                    {
                        // `attach` sends the exit itself when the shell is
                        // already gone, so the code is on the wire by here — and
                        // the notice is in the scrollback it just replayed.
                        conn = Some(c);
                        delivered = true;
                    }
                }
                Some(Cmd::Disconnected(seq)) => {
                    if conn.as_ref().is_some_and(|c| c.generation == seq) {
                        conn = None;
                        // The daemon took the exit and closed, which is the
                        // normal end of a holder's life.
                        if delivered {
                            return;
                        }
                    }
                }
                // Frames after the shell has exited change nothing: there is no
                // PTY left to write to or resize, and the hangup already
                // happened.
                Some(Cmd::Frame(..)) => {}
                None => return,
            }
        }
    })
    .await;

    info!(session = %cfg.session_id, code, delivered, "terminal holder exiting");
}

/// Accept connections for the life of the holder.
async fn acceptor(listener: UnixListener, tx: mpsc::Sender<Cmd>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                if tx.send(Cmd::Connected(stream)).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                // A per-connection failure (EMFILE, ECONNABORTED) must not spin
                // this loop at full speed.
                warn!("holder accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

/// Greet a newly connected daemon and start pumping its connection.
///
/// Returns `None` if the greeting could not be delivered, in which case the
/// connection is dropped rather than half-established — a daemon that never got
/// the `Hello` cannot make sense of anything that follows it.
#[allow(clippy::too_many_arguments)]
async fn attach(
    stream: UnixStream,
    generation: u64,
    cmd_tx: mpsc::Sender<Cmd>,
    cfg: &HolderConfig,
    pid: i32,
    exited: Option<u32>,
    scrollback: &Scrollback,
    disconnected_since: Option<Instant>,
) -> Option<Conn> {
    let (reader, mut writer) = stream.into_split();

    // The reader starts **before** the greeting is written, and this ordering is
    // load-bearing rather than tidy.
    //
    // A peer is allowed to connect, write `HANGUP`, and close — that is the whole
    // point of pinning one frame across every protocol version, and three callers
    // now do exactly it (`veld uninstall`'s sweep, the recovery test's cleanup
    // guard, and the daemon refusing an unknown protocol version). Greeting first
    // made every one of them silently ineffective: the peer had already closed, so
    // our `HELLO` write failed EPIPE, this function returned before spawning a
    // reader, and the `HANGUP` sat unread in the receive buffer while the shell
    // kept running — with a log line claiming it had been asked to stop.
    let (out, rx) = mpsc::channel::<(u8, Vec<u8>)>(OUT_CHANNEL);
    tokio::spawn(pump_reader(reader, generation, cmd_tx));

    let hello = wire::Hello {
        protocol: wire::PROTOCOL,
        session_id: cfg.session_id.clone(),
        worktree_id: cfg.worktree_id,
        label: cfg.label.clone(),
        cwd: cfg.cwd.display().to_string(),
        pid,
        exited,
        // Measured from *this* holder's clock, not the daemon's: the daemon that
        // is connecting may never have seen this session before.
        detached_secs: disconnected_since
            .map(|since| Instant::now().saturating_duration_since(since).as_secs()),
    };
    let encoded = serde_json::to_vec(&hello).ok()?;
    // Bounded, because `attach` is awaited from the main loop: a scrollback can be
    // a quarter of a megabyte against an ~8 KB socket buffer, so a peer that does
    // not read would otherwise park the loop — and with it the very `HANGUP` that
    // peer may have just sent — until it went away on its own.
    let greeted = tokio::time::timeout(GREETING_TIMEOUT, async {
        wire::write_frame(&mut writer, wire::HELLO, &encoded).await?;
        wire::write_frame(&mut writer, wire::SCROLLBACK, &scrollback.snapshot()).await?;
        if let Some(code) = exited {
            // A daemon adopting an already-finished session must be able to report
            // the exit instead of presenting a dead prompt as a live one.
            wire::write_frame(&mut writer, wire::EXIT, &code.to_be_bytes()).await?;
        }
        Ok::<(), std::io::Error>(())
    })
    .await;
    match greeted {
        Ok(Ok(())) => {}
        // Either way there is no usable connection. The reader spawned above is
        // still running, so anything the peer sent — a `HANGUP` in particular — is
        // delivered before it reports the disconnect.
        Ok(Err(e)) => {
            debug!(session = %cfg.session_id, "greeting failed: {e}");
            return None;
        }
        Err(_) => {
            warn!(
                session = %cfg.session_id,
                "greeting timed out after {}s — the peer is not reading",
                GREETING_TIMEOUT.as_secs()
            );
            return None;
        }
    }

    tokio::spawn(pump_writer(writer, rx));
    debug!(session = %cfg.session_id, generation, "daemon attached to holder");
    Some(Conn { generation, out })
}

/// Forward queued frames to the daemon until the channel or the socket ends.
async fn pump_writer(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut rx: mpsc::Receiver<(u8, Vec<u8>)>,
) {
    while let Some((kind, payload)) = rx.recv().await {
        if wire::write_frame(&mut writer, kind, &payload)
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Decode the daemon's frames into [`Cmd`]s until the connection ends.
async fn pump_reader(
    mut reader: tokio::net::unix::OwnedReadHalf,
    generation: u64,
    tx: mpsc::Sender<Cmd>,
) {
    loop {
        match wire::read_frame(&mut reader).await {
            Ok(Some(frame)) => {
                if tx.send(Cmd::Frame(generation, frame)).await.is_err() {
                    return;
                }
            }
            Ok(None) => break,
            Err(e) => {
                warn!("holder connection read failed: {e}");
                break;
            }
        }
    }
    let _ = tx.send(Cmd::Disconnected(generation)).await;
}

// ---------------------------------------------------------------------------
// PTY
// ---------------------------------------------------------------------------

struct Spawned {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    pid: i32,
    read_fd: AsyncFd<File>,
    write_fd: AsyncFd<File>,
}

/// Open a PTY and start the user's login shell in `cwd`.
fn spawn_shell(cwd: &std::path::Path, size: PtySize) -> anyhow::Result<Spawned> {
    let PtyPair { master, slave } = native_pty_system().openpty(size)?;

    let shell = login_shell();
    let mut cmd = CommandBuilder::new(&shell);
    // A *login* shell, which is also what makes this an exception to the
    // AGENTS.md "resolve the user's PATH with `resolve_user_path()`" rule.
    // That helper exists because a daemon running `sh -c '<config command>'`
    // inherits launchd's bare PATH; it gets the real one by spawning
    // `$SHELL -l -i -c 'command env'` and scraping it. Here the thing being
    // spawned *is* that login shell, so it computes the same PATH itself —
    // calling the helper first would spawn a second shell and add its startup
    // cost (up to its 10s timeout on a wedged rc file) to every terminal, to
    // arrive at the value this shell is about to compute anyway.
    cmd.arg("-l");
    cmd.cwd(cwd);
    // xterm.js speaks xterm-256color; without TERM the shell assumes "dumb"
    // and disables colour and line editing.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let child = slave.spawn_command(cmd)?;
    // Close our copy of the slave. While this process holds it, the master
    // never reaches EOF after the shell exits, and the session would hang
    // until its drain timer instead of ending cleanly.
    drop(slave);

    let pid = child.process_id().unwrap_or(0) as i32;
    let raw = master
        .as_raw_fd()
        .ok_or_else(|| anyhow::anyhow!("pty master has no file descriptor"))?;

    // Two independent descriptors so the read and write loops cannot block
    // each other. They share one open file description, so O_NONBLOCK set on
    // either applies to both — which is what we want, and why the writer is
    // also driven through AsyncFd rather than blocking on the shared flag.
    let read_fd = async_dup(raw)?;
    let write_fd = async_dup(raw)?;

    Ok(Spawned {
        master,
        child,
        pid,
        read_fd,
        write_fd,
    })
}

/// The user's shell, falling back to a POSIX shell that is present on every
/// supported platform. `SHELL` comes from this process's environment, inherited
/// from the daemon (launchd/systemd propagate the user's), never from a client.
fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

/// Push a new window size into the kernel.
fn resize(master: &dyn MasterPty, cols: u16, rows: u16) {
    let size = PtySize {
        cols: clamp_dimension(Some(cols), 80),
        rows: clamp_dimension(Some(rows), 24),
        pixel_width: 0,
        pixel_height: 0,
    };
    debug_assert!(size.cols <= MAX_DIMENSION && size.rows <= MAX_DIMENSION);
    if let Err(e) = master.resize(size) {
        debug!("terminal resize failed: {e}");
    }
}

/// Duplicate a descriptor, mark it non-blocking, and hand it to tokio's
/// readiness machinery.
///
/// A duplicate rather than the master's own descriptor because `AsyncFd`
/// wants ownership, while `master` must stay alive to serve resizes.
fn async_dup(raw: RawFd) -> anyhow::Result<AsyncFd<File>> {
    // SAFETY: `raw` is owned by the live `MasterPty` for the duration of this
    // call; F_DUPFD_CLOEXEC returns a new descriptor we own outright, and
    // close-on-exec keeps it out of any process spawned from another thread
    // between here and the `OwnedFd`.
    let dup = unsafe { libc::fcntl(raw, libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `dup` is a fresh descriptor with no other owner.
    let owned = unsafe { OwnedFd::from_raw_fd(dup) };

    // SAFETY: `owned` keeps the descriptor alive across both calls.
    let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    Ok(AsyncFd::new(File::from(owned))?)
}

/// Read PTY output, waiting for readiness. `Ok(0)` means hangup.
async fn pty_read(fd: &AsyncFd<File>, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        let mut guard = fd.readable().await?;
        match guard.try_io(|inner| {
            let mut file = inner.get_ref();
            file.read(buf)
        }) {
            Ok(result) => return result,
            // Readiness was stale; wait for it again.
            Err(_would_block) => continue,
        }
    }
}

/// Write all of `data` to the PTY, waiting for writability between partial
/// writes.
async fn pty_write(fd: &AsyncFd<File>, data: &[u8]) -> std::io::Result<()> {
    let mut rest = data;
    while !rest.is_empty() {
        let mut guard = fd.writable().await?;
        match guard.try_io(|inner| {
            let mut file = inner.get_ref();
            file.write(rest)
        }) {
            // A pty master accepts zero bytes only if it is gone; looping on
            // it would spin forever.
            Ok(Ok(0)) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(Ok(n)) => rest = &rest[n..],
            Ok(Err(e)) => return Err(e),
            Err(_would_block) => continue,
        }
    }
    Ok(())
}

/// Hang up the terminal's process group, the way closing a real terminal does.
///
/// `portable-pty` puts the shell in its own session (`setsid`) with the PTY as
/// its controlling terminal, so its process-group id equals its pid and
/// `killpg` reaches the shell together with whatever job is in the foreground.
/// A shell that honours SIGHUP hangs up its background jobs on the way out —
/// which is why this is preferable to signalling the shell alone
/// (`ChildKiller::kill`, which sends SIGHUP to the pid only).
fn hangup(pid: i32) {
    signal_group(pid, libc::SIGHUP);
}

/// Kill the terminal's process group outright.
fn kill(pid: i32) {
    signal_group(pid, libc::SIGKILL);
}

fn signal_group(pid: i32, sig: i32) {
    // A non-positive pid would be catastrophic here: `killpg(0, …)` signals
    // *this process's own* group. `process_id()` returning None lands on 0, so
    // this guard is load-bearing, not defensive dressing.
    if pid <= 0 {
        warn!("refusing to signal terminal process group {pid}");
        return;
    }
    // SAFETY: `killpg` is async-signal-safe and takes no pointers; a pid that
    // has already been reaped simply yields ESRCH, which we ignore.
    if unsafe { libc::killpg(pid, sig) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::ESRCH) {
            debug!("killpg({pid}, {sig}) failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Losing a daemon after the orphan grace has *already elapsed while
    /// attached* must start a fresh grace, not hang up immediately.
    ///
    /// This is a regression test for a fix, not for the feature: an armed
    /// `sleep(grace)` created once at startup elapses on its own schedule whether
    /// or not its `select!` branch is being polled, so the naive version killed
    /// the shell the instant a long-lived terminal lost its daemon — i.e. on
    /// every `veld update` of a terminal older than the grace, which is the exact
    /// thing this module exists to prevent. The shell survives here only if
    /// disconnecting re-arms the deadline.
    #[tokio::test]
    async fn a_disconnect_after_the_grace_has_elapsed_starts_a_new_grace() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("s.sock");
        let grace = 1;
        let cfg = HolderConfig {
            session_id: "graceprobe".to_owned(),
            worktree_id: 1,
            label: "test".to_owned(),
            cwd: dir.path().to_path_buf(),
            cols: 80,
            rows: 24,
            socket: socket.clone(),
            orphan_grace_secs: grace,
        };
        tokio::spawn(run(cfg));

        // Connect, and stay connected well past the grace.
        let mut stream = loop {
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(s) => break s,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        let hello = wire::read_frame(&mut stream).await.unwrap().unwrap();
        assert_eq!(hello.kind, wire::HELLO);
        let pid = serde_json::from_slice::<wire::Hello>(&hello.payload)
            .unwrap()
            .pid;
        tokio::time::sleep(Duration::from_secs(grace) * 3).await;

        // Now lose the daemon. The shell must still be there afterwards: the
        // clock starts *now*.
        drop(stream);
        tokio::time::sleep(Duration::from_millis(400)).await;
        // SAFETY: signal 0 performs the permission/existence check only.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "the shell must survive a disconnect that happens after the grace \
             already elapsed while a daemon was attached"
        );

        // And the grace must still be enforced from that disconnect.
        let deadline = Instant::now() + Duration::from_secs(grace) + Duration::from_secs(20);
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "pid {pid} outlived its re-armed orphan grace"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// A peer may connect, write the five bytes of `HANGUP`, and close
    /// immediately — the contract `veld uninstall`'s sweep, the recovery test's
    /// cleanup guard and the version-refusal path all rely on.
    ///
    /// It did not work: the holder wrote its greeting first, that write failed
    /// EPIPE against a peer that had already gone, and the function returned
    /// before spawning the reader that would have seen the hangup. All three
    /// callers logged success while the shell kept running. The frame is written by
    /// hand here for the same reason those callers write it by hand — it is
    /// supposed to need nothing but five bytes.
    #[tokio::test]
    async fn a_hangup_written_by_a_peer_that_closes_still_ends_the_shell() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("h.sock");
        let cfg = HolderConfig {
            session_id: "hangupprobe".to_owned(),
            worktree_id: 1,
            label: "test".to_owned(),
            cwd: dir.path().to_path_buf(),
            cols: 80,
            rows: 24,
            socket: socket.clone(),
            // Long, so nothing but the hangup can be what ends this shell.
            orphan_grace_secs: 3600,
        };
        tokio::spawn(run(cfg));

        // Learn the pid from a first, well-behaved connection, then drop it.
        let pid = {
            let mut stream = loop {
                match tokio::net::UnixStream::connect(&socket).await {
                    Ok(s) => break s,
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            };
            let hello = wire::read_frame(&mut stream).await.unwrap().unwrap();
            serde_json::from_slice::<wire::Hello>(&hello.payload)
                .unwrap()
                .pid
        };

        // The hangup-only peer: connect, write, close. It never reads a byte.
        {
            let mut blunt = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            let mut frame = vec![0x83u8];
            frame.extend_from_slice(&0u32.to_be_bytes());
            blunt.write_all(&frame).unwrap();
            blunt.flush().unwrap();
        }

        let deadline = Instant::now() + KILL_GRACE + Duration::from_secs(20);
        // SAFETY: signal 0 performs the permission/existence check only.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                Instant::now() < deadline,
                "pid {pid} ignored a hangup from a peer that closed immediately"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[test]
    fn the_exit_notice_is_a_complete_line() {
        let notice = String::from_utf8(exit_notice(7)).unwrap();
        // A notice without the leading CRLF lands on top of whatever the shell
        // last printed, and one without the trailing pair leaves the client's
        // cursor mid-line.
        assert!(notice.starts_with("\r\n"));
        assert!(notice.ends_with("\r\n"));
        assert!(notice.contains("shell exited (7)"));
    }

    #[tokio::test]
    async fn binding_refuses_to_displace_a_live_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let first = bind(&path).unwrap();

        // Two holders for one session would fight over one shell.
        let err = bind(&path).expect_err("must refuse a live socket");
        assert!(err.to_string().contains("already listening"));
        drop(first);
    }

    #[tokio::test]
    async fn binding_replaces_a_stale_socket_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        {
            let _dead = bind(&path).unwrap();
        }
        // The file outlives a killed holder; refusing to bind here would make a
        // session id permanently unusable.
        assert!(path.exists());
        let _live = bind(&path).unwrap();
    }

    #[tokio::test]
    async fn the_socket_and_its_directory_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("pty");
        let path = nested.join("s.sock");
        let _l = bind(&path).unwrap();
        // Every socket in here is a shell; group or world access would hand one
        // to another local account.
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&nested), 0o700);
    }
}
