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

/// How long a newly accepted connection must stay open, while another daemon is
/// already attached, before it is allowed to take the session over.
///
/// **This is what stops a liveness probe from killing a terminal.** Connecting
/// to a holder is how everything decides whether one is alive: `veld doctor`
/// walks the socket directory, and [`bind`] connects to a path already in use to
/// tell a leftover file from a live holder. Both connect and close immediately.
/// While *any* accepted connection displaced the attached daemon on the spot,
/// each of those probes severed a live session — the daemon read EOF, published
/// an exit for a shell that was still running, and the reaper eventually hung
/// that shell up. One `veld doctor` did it to every terminal on the machine.
///
/// A real peer, by contrast, connects and stays. So a newcomer is greeted at
/// once (which is what keeps the hangup-and-close contract in [`wire`] working)
/// but does not displace the incumbent until it has been connected this long.
/// Short enough that a genuine takeover is imperceptible, and four orders of
/// magnitude longer than a probe's connection lives.
const TAKEOVER_PROBATION: Duration = Duration::from_secs(1);

/// How long [`redraw`] leaves the terminal one row short before restoring it.
///
/// Standard signals coalesce, so two `TIOCSWINSZ` calls with no gap wake a
/// program once and it reads only the final size — for a renderer that diffs its
/// own output, an identical frame and nothing written. Measured through the real
/// daemon against such a renderer: a 0 ms gap redrew 0 frames, 5 ms redrew 2.
/// This is that floor with margin, and it is the whole visible cost of a
/// reattach — one stale bottom row, once, which is imperceptible anywhere in this
/// range.
///
/// **Best-effort, and it cannot be otherwise.** The gap only works if the program
/// is scheduled and renders inside it, so a machine loaded enough to starve it for
/// this long collapses back into the 0 ms case: both signals coalesce, the program
/// reads only the final size, and the repaint silently does not happen. There is
/// no retry and no verification — the daemon cannot tell a program that ignored
/// the signal from one that had nothing to redraw. The value below is chosen for
/// margin against scheduler latency, not because it is a *bound* on it, and the
/// failure mode is the pre-existing one (a screen that needs a manual resize), not
/// a worse one. Raise it before suspecting anything subtler if the repaint starts
/// missing under load.
///
/// Bounded, and that is what makes awaiting it in the control loop acceptable
/// where the output path needs [`OUTPUT_SEND_TIMEOUT`]: the hazard there is a peer
/// that never reads, i.e. *unbounded* parking, which would take `HANGUP` handling
/// down with it. This is one fixed gap, once per resumed attach.
///
/// Deliberately not restated as a figure anywhere above: a comment that repeats
/// the value drifts from it silently, and this one already had. The number lives on
/// the line below and nowhere else.
///
/// It parks the whole `select!` for that window, not just the next daemon frame —
/// the PTY-read branch included, so the repaint the nudge provokes waits in the
/// kernel buffer until the restore is done. That is harmless at this size and is
/// worth knowing before adding a second blocking step nearby.
///
/// Parking is also what makes the window *safe* rather than merely tolerable: a
/// client resize arriving mid-nudge cannot race the restore, because it queues on
/// `to_holder` and is applied strictly after it. Measured — a resize sent inside
/// the window left the pty at the client's size, not the restored one. Moving this
/// sleep off the control loop would turn that into a real race and could leave a
/// terminal permanently one row short.
const REDRAW_NUDGE: Duration = Duration::from_millis(80);

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
///
/// It is a connect-and-close, which is a *probe* and not a takeover — the live
/// holder on the other side must not treat it as one. See
/// [`TAKEOVER_PROBATION`]: while it did, this very check severed that holder's
/// daemon connection, and the daemon then reported a live shell as exited. The
/// spawn-then-fail-to-bind path this runs on is reached on every resume of a
/// session whose holder outlived its daemon link, so it fired in a loop.
fn bind(path: &std::path::Path) -> anyhow::Result<UnixListener> {
    // Before anything else, because `bind`'s own answer to this is "path must be
    // shorter than SUN_LEN" — true, and it names neither the path nor the way
    // out. It reaches the user as "could not open a terminal".
    if let Some(msg) = veld_core::instance::socket_path_too_long(path) {
        anyhow::bail!(msg);
    }
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
    /// way a second WebSocket takes over from the first — but only once it has
    /// held the connection open for [`TAKEOVER_PROBATION`], because a bare
    /// connect is also how everything probes whether this holder is alive.
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
    } = spawn_shell(
        &cfg.cwd,
        size,
        cfg.argv.as_deref(),
        // The daemon's answer when it has one, and this process's own only when it
        // does not — an older daemon's holder config carries no `shell_argv`. See
        // [`HolderConfig::shell_argv`].
        &cfg.shell_argv
            .clone()
            .unwrap_or_else(|| vec![login_shell(), "-l".to_owned()]),
        &cfg.env,
    )
    .context("failed to open a pty")?;
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
    // Greeted, but not yet trusted with the session — see `TAKEOVER_PROBATION`.
    // Only ever occupied while `conn` is, because a newcomer arriving with
    // nothing attached has nothing to displace and is promoted immediately.
    let mut pending: Option<Conn> = None;
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
    // Placeholder deadline, armed only while a probationary connection exists,
    // for the same reason `drain` above is a `sleep` and not an `interval`. An
    // elapsed `Sleep` polls `Ready` forever, so the branch is guarded on
    // `pending.is_some()` as well — and every arming resets it.
    let probation = tokio::time::sleep(Duration::from_secs(3600));
    tokio::pin!(probation);

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
                    // A probationary peer is fed the same chunk, which is what
                    // makes promoting it lossless. Its greeting carried a
                    // scrollback snapshot taken when it connected, and nothing
                    // re-sends one — so without this, everything the shell printed
                    // during the probation would be missing from the promoted
                    // daemon's mirror for good, possibly splitting an escape
                    // sequence. `try_send`, unlike the incumbent's send below: a
                    // peer that cannot take a chunk with `OUT_CHANNEL` frames of
                    // slack is not a peer to hand a live session to, and a probe
                    // that never reads must not be able to slow the shell down.
                    if let Some(p) = &pending {
                        if p.out.try_send((wire::OUTPUT, buf[..n].to_vec())).is_err() {
                            debug!(session = %cfg.session_id, "dropping a probationary peer that is not keeping up");
                            pending = None;
                        }
                    }
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
                            conn = pending.take();
                            if conn.is_none() {
                                mark_disconnected!();
                            }
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
                    let greeted = attach(
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
                    match greeted {
                        // Nothing attached: this newcomer is the one and only.
                        Some(c) if conn.is_none() => {
                            conn = Some(c);
                            // A newcomer arriving with nothing attached cannot be
                            // waiting behind anything, so any `pending` here would
                            // be a leftover of a state that is not supposed to
                            // exist. Dropped rather than kept, so "at most one
                            // probationary connection" stays true by construction.
                            pending = None;
                            disconnected_since = None;
                        }
                        // A daemon is attached. Greet this one — a peer is allowed
                        // to connect only to write `HANGUP`, and that still works
                        // from here — but do not hand it the session until it has
                        // proved it is a peer at all. See `TAKEOVER_PROBATION`.
                        Some(c) => {
                            // The deadline is armed only when the slot goes from
                            // empty to occupied. Re-arming it for a *replacement*
                            // would mean anything that reconnects faster than the
                            // window keeps a takeover from ever completing — and
                            // replacing is otherwise the same last-one-wins rule
                            // takeover has always had, applied one step earlier.
                            if pending.is_none() {
                                probation
                                    .as_mut()
                                    .reset(tokio::time::Instant::now() + TAKEOVER_PROBATION);
                            }
                            pending = Some(c);
                        }
                        // The greeting failed, so nothing changed: whoever was
                        // attached still is, and if nobody was, the orphan clock
                        // that started when they left keeps running. Re-arming it
                        // here is what let a stream of failed connections — a
                        // poll-connect against an over-cap daemon, a monitor —
                        // keep an abandoned shell alive indefinitely.
                        None => {}
                    }
                }
                Cmd::Frame(seq, frame) => {
                    // A probationary peer that *speaks* has proved itself sooner
                    // than the window could: only a daemon sends input, a resize or
                    // a redraw, and the probes the window exists to survive send
                    // nothing at all. (`REDRAW` earns its place here defensively
                    // rather than because anything reaches it as a first frame
                    // today — see the drop-exemption list further down.) Promoting here rather than making it wait is what keeps
                    // a takeover prompt — this very frame is then acted on below
                    // instead of being dropped as a displaced peer's, and the
                    // output it missed while waiting was teed to it by the PTY
                    // branch above, which is what makes it lossless. `HANGUP` is
                    // deliberately not in the list: it ends a session rather than
                    // writing to it, and its whole contract is that it works
                    // without being anybody's writer.
                    if pending.as_ref().is_some_and(|c| c.generation == seq)
                        && matches!(frame.kind, wire::INPUT | wire::RESIZE | wire::REDRAW)
                    {
                        info!(session = %cfg.session_id, "a second daemon took the session over");
                        conn = pending.take();
                        disconnected_since = None;
                    }
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
                            wire::REDRAW => {
                                // Two calls with a gap between them, not one: the
                                // gap is what makes the change observable (see
                                // `redraw_nudge`), and the borrow of `master`
                                // cannot span the await.
                                if let Some(size) = redraw_nudge(master.as_ref()) {
                                    tokio::time::sleep(REDRAW_NUDGE).await;
                                    resize(master.as_ref(), size.cols, size.rows);
                                }
                            }
                            wire::QUERY_BUSY => {
                                // The answer rides the *current* connection's out
                                // queue; the daemon that asked is the one attached
                                // (the guard above proved the generation).
                                //
                                // `try_send`, not `send().await`: this branch is the
                                // holder's control loop, and a blocking send on a full
                                // out channel would park it — taking `HANGUP` handling
                                // down with it, the exact failure the OUTPUT path's
                                // timeout exists to prevent. A busy reply is not screen
                                // data; dropping it is fine, because the daemon's own
                                // query times out and reports idle.
                                if let Some(c) = conn.as_ref() {
                                    // A config-declared pane runs `$SHELL -l -i -c
                                    // '<command>'`, and an interactive shell **execs** a
                                    // simple command into its own process group rather
                                    // than giving it one of its own — so `tcgetpgrp`
                                    // equals `pid` for the whole life of a running pane
                                    // (the git-log pane, a coding agent), and the pgrp
                                    // heuristic below would read it as *idle* while it
                                    // is busy. For a pane, "busy" simply means its
                                    // command has not exited yet.
                                    let busy = if cfg.argv.is_some() {
                                        exit_code.is_none()
                                    } else {
                                        session_busy(master.as_ref(), pid)
                                    };
                                    let reply =
                                        (wire::BUSY, wire::encode_busy(busy).to_vec());
                                    if c.out.try_send(reply).is_err() {
                                        debug!("dropping a busy reply: the daemon is behind");
                                    }
                                }
                            }
                            other if frame.is_ignorable() => {
                                debug!("ignoring holder-numbered frame {other:#x}");
                            }
                            other => {
                                warn!("dropping connection: unsupported frame {other:#x}");
                                conn = pending.take();
                                if conn.is_none() {
                                    mark_disconnected!();
                                }
                            }
                        }
                    }
                    // A probationary peer's frames are not acted on either — it is
                    // not the writer — but one it cannot be served changes nothing
                    // except that it is not a peer worth promoting.
                    else if pending.as_ref().is_some_and(|c| c.generation == seq)
                        && !frame.is_ignorable()
                        // `REDRAW` is in both this list and the promotion one above, and in
                        // *neither* does it change what happens today: no path in
                        // `serve_socket` reaches its `redraw_session` without having already
                        // awaited `resize_session`, and both frames ride this one ordered
                        // channel, so that `RESIZE` promotes a probationary peer before its
                        // `REDRAW` is ever read. Which is also what makes this arm
                        // unreachable for a `REDRAW` — promotion took `pending`.
                        //
                        // Both entries are insurance against precisely the cleanup this
                        // module's own docs invite. The `RESIZE` they lean on carries the
                        // size the pty already has, and `redraw_nudge` argues at length that
                        // such a resize signals nothing — so somebody will eventually delete
                        // it as dead work, and on that day a `REDRAW` becomes an adopted
                        // peer's first frame. With the promotion entry it still works;
                        // without it the repaint is lost in silence; without either, the
                        // peer loses its takeover too. The promotion half is pinned by
                        // `a_peer_whose_only_frame_is_a_redraw_takes_the_session_over_at_once`
                        // so the insurance cannot be quietly removed; this entry is kept in
                        // step with it.
                        && !matches!(frame.kind, wire::INPUT | wire::RESIZE | wire::REDRAW)
                    {
                        warn!(
                            "dropping a probationary connection: unsupported frame {:#x}",
                            frame.kind
                        );
                        pending = None;
                    }
                }
                Cmd::Disconnected(seq) => {
                    if conn.as_ref().is_some_and(|c| c.generation == seq) {
                        debug!(session = %cfg.session_id, "daemon disconnected");
                        // Whatever was waiting behind it takes over now rather
                        // than serving its probation out: the point of the wait is
                        // to protect the *incumbent*, and there no longer is one.
                        conn = pending.take();
                        if conn.is_none() {
                            mark_disconnected!();
                        }
                    } else if pending.as_ref().is_some_and(|c| c.generation == seq) {
                        // A probationary connection went away before proving
                        // itself — a `veld doctor` probe, or a holder that found
                        // this socket occupied and gave up. The attached daemon is
                        // untouched, which is the entire point of the probation.
                        debug!(session = %cfg.session_id, "a probationary peer disconnected");
                        pending = None;
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

            // Still connected after the probation window, so this is a peer and
            // not a probe: hand it the session, which drops the incumbent's
            // connection exactly as an immediate takeover always did.
            _ = &mut probation, if pending.is_some() => {
                info!(session = %cfg.session_id, "a second daemon took the session over");
                conn = pending.take();
                disconnected_since = None;
            },

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
    let notice = exit_notice(code, cfg.pane_label.as_deref());
    scrollback.push(&notice);
    debug!(session = %cfg.session_id, pid, code, "terminal shell exited");

    // A peer still on probation is told too, and `deliver_exit` below cannot do it
    // — it takes the one connection that owns the session. `pending` is only ever
    // occupied while `conn` is (it is assigned nowhere else, and every path that
    // clears `conn` takes it first), so this is never the *only* peer; it is a
    // second daemon that would otherwise read the close as its holder vanishing
    // and invent an exit code of its own for a shell that ended with a real one.
    //
    // `try_send`, and best-effort: a peer that cannot take two frames with
    // `OUT_CHANNEL` of slack is not reading at all, and the shell is already gone.
    // Dropping the sender afterwards is what ends its writer task — an `mpsc`
    // drains what is queued before reporting the close.
    if let Some(p) = pending.take() {
        let _ = p.out.try_send((wire::OUTPUT, notice.to_vec()));
        let _ = p.out.try_send((wire::EXIT, code.to_be_bytes().to_vec()));
    }

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

/// The line a client sees in place of a prompt once the session's process is
/// gone.
///
/// Named after what actually ran: a config-declared pane never spawned a shell,
/// and after a full-screen program exits this line is frequently the *only*
/// thing on the pane, because restoring the primary screen buffer wipes
/// everything the user was looking at.
fn exit_notice(code: u32, label: Option<&str>) -> Vec<u8> {
    let what = label.unwrap_or("shell");
    format!("\r\n\x1b[2m[veld] {what} exited ({code})\x1b[0m\r\n").into_bytes()
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
                        // the notice is in the scrollback it just replayed. That
                        // makes greeting a newcomer *sufficient* here, which is why
                        // this arm needs no probation: what it must not do is
                        // **displace** the peer that is already holding the exit
                        // open, because that peer reads the close as its holder
                        // vanishing. A liveness probe landing inside this window
                        // used to do exactly that.
                        delivered = true;
                        if conn.is_none() {
                            conn = Some(c);
                        }
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
        // This build answers QUERY_BUSY; an older holder that cannot would
        // drop the connection if asked, so the daemon gates on this flag.
        supports_busy: true,
        // This build acts on REDRAW; an older holder that cannot would drop the
        // connection if asked, so the daemon gates on this flag.
        supports_redraw: true,
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

/// Open a PTY and start the session's process in `cwd`.
///
/// Two shapes, and the difference decides who computes `PATH`:
///
/// - **No `argv`** — `shell_argv`, which is the ordinary terminal. The daemon
///   resolved it from `terminal.shell` and chose its flags (see
///   [`wire::HolderConfig::shell_argv`]); this process only spawns it.
/// - **An `argv`** — a config-declared pane's command. `resolve_pane` has
///   already wrapped it in the user's **login+interactive** shell (`<shell> -l -i
///   -c '<command>'`), so this is what actually runs here; the pane command
///   inherits the same environment a real terminal gives, and `PATH` is the
///   login shell's own to compute. The daemon still injects a resolved `PATH`
///   as a floor for a shell with no rc files. See [`wire::HolderConfig::env`].
fn spawn_shell(
    cwd: &std::path::Path,
    size: PtySize,
    argv: Option<&[String]>,
    shell_argv: &[String],
    env: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<Spawned> {
    let PtyPair { master, slave } = native_pty_system().openpty(size)?;

    let mut cmd = match argv {
        Some([program, args @ ..]) => {
            let mut cmd = CommandBuilder::new(program);
            cmd.args(args);
            cmd
        }
        // An empty argv cannot reach here (the config parser refuses one and the
        // daemon re-checks), but falling back to a shell beats panicking in a
        // process whose whole job is to outlive things.
        Some([]) | None => {
            // A *login* shell — the daemon put the `-l` (and, for a bash with the
            // `$ENV` handoff, the leading `--posix`) in `shell_argv`. That is also
            // what makes this an exception to the AGENTS.md "resolve the user's
            // PATH with `resolve_user_path()`" rule. That helper exists because a
            // daemon running `sh -c '<config command>'` inherits launchd's bare
            // PATH; it gets the real one by spawning `<shell> -l -i -c 'command
            // env'` and scraping it. Here the thing being spawned *is* that login
            // shell, so it computes the same PATH itself — calling the helper first
            // would spawn a second shell and add its startup cost (up to its 10s
            // timeout on a wedged rc file) to every terminal, to arrive at the
            // value this shell is about to compute anyway.
            // An empty or program-less `shell_argv` cannot reach here — the daemon
            // always sends one and the fallback above builds one — but falling back
            // to a plain login shell beats panicking in a process whose whole job is
            // to outlive things.
            match shell_argv.split_first() {
                Some((program, args)) if !program.is_empty() => {
                    let mut cmd = CommandBuilder::new(program);
                    cmd.args(args);
                    cmd
                }
                _ => {
                    let mut cmd = CommandBuilder::new(login_shell());
                    cmd.arg("-l");
                    cmd
                }
            }
        }
    };
    cmd.cwd(cwd);
    // xterm.js speaks xterm-256color; without TERM the shell assumes "dumb"
    // and disables colour and line editing.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // The daemon's additions (`$BROWSER` and friends — see `pty::shims`, plus the
    // resolved `PATH` a pane command needs). Set last, after TERM/COLORTERM so a
    // pane can override them, and never before `cwd`, which is the daemon's to
    // decide either way.
    //
    // Note what this deliberately does **not** try to be for the *shell* path: a
    // login shell is free to overwrite any of these in its rc files, and `PATH` in
    // particular is rewritten by `path_helper` on macOS and by `/etc/profile` on
    // Debian. That is why nothing here relies on `PATH` order; see
    // `veld_core::opener`. For the `argv` path there is no rc file in the way, so
    // the injected `PATH` is the one the command actually gets — which is the
    // whole point of injecting it.
    for (key, value) in env {
        cmd.env(key, value);
    }

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

/// This process's own idea of the user's shell — `$SHELL`, then the `passwd`
/// entry, then `/bin/sh`.
///
/// **Only the fallback.** Which shell a terminal opens is a user preference
/// (`terminal.shell`), and the preference lives in the database, which this
/// process deliberately does not open: a holder is a dumb PTY owner that must
/// outlive the daemon, and `Db::open()` on the session-spawn path is the thing
/// AGENTS.md warns about. So the daemon resolves the shell once, at ticket-mint
/// time, and sends it in [`HolderConfig::shell_argv`]. This is what a holder spawned by
/// an older daemon — one whose config carries no `shell_argv` — falls back to, which is
/// exactly the behaviour that daemon had.
pub fn login_shell() -> String {
    veld_core::shell::auto_shell()
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

/// Make the foreground program repaint its screen.
///
/// A full-screen program — a coding agent, vim, a pager — owns every cell it
/// draws and only redraws when something tells it to. Nothing does, when a
/// browser reattaches to a shell that never stopped: the replayed scrollback
/// lands on top of whatever the screen already held, and the program has no
/// idea, so the mixture stays until the user happens to drag the pane and the
/// resize repaints it. That drag is the whole bug report.
///
/// # Why a size change and not a bare `SIGWINCH`
///
/// The drag delivers `SIGWINCH`, so sending one directly is the obvious fix, and
/// it is not enough. A renderer that *diffs* — ink/`log-update`, which is what a
/// coding agent's TUI is built on, and `ratatui`'s `Terminal::autoresize` — takes
/// the signal, recomputes its frame from the size it reads, finds it byte-identical
/// to the frame it last wrote, and writes **nothing**. Measured through the real
/// daemon against a renderer of that shape: a bare same-size signal produced zero
/// bytes, while `rows - 1` then `rows` produced two full repaints. Only programs
/// that repaint unconditionally (vim) are woken by the signal alone, so the bare
/// version fixes the case nobody reported and misses the case everybody did.
///
/// So this changes the size and puts it back — the redraw method `dtach` and
/// `abduco` settled on for the same reason. Re-sending the *same* size cannot
/// work either: [`resize`] issues `TIOCSWINSZ` unconditionally, but Linux
/// (`tty_do_resize`) and XNU (`ttioctl_locked`) compare the new `winsize` with
/// the old and skip the signal when they match, which is every reattach at an
/// unchanged pane size.
///
/// # Why the gap is load-bearing
///
/// [`REDRAW_NUDGE`] is not padding. Standard signals coalesce, so with the two
/// `ioctl`s back to back the program is woken once and reads only the *final*
/// size — identical frame, nothing written. Measured, one variable, same probe:
/// a 0 ms gap redrew **0** frames while 5 ms redrew 2. The value here is that
/// floor plus margin for an event loop busy rendering.
///
/// Signalling is left to the kernel rather than done here with `killpg`. That is
/// not only simpler: the kernel signals the foreground process group under the
/// tty lock, holding the group itself, whereas reading a pgid with `tcgetpgrp`
/// and then signalling that *number* can land on a group that was reaped and its
/// id reused in between — and it would have been this module's only `killpg` at a
/// group it neither owns nor reaps, against the rule its own header sets out.
///
/// Split across the gap rather than written as one `async fn` because a
/// `&dyn MasterPty` is not `Send` and this runs inside a spawned task, so the
/// borrow must not span the await. This half applies the nudge and hands back the
/// size to restore; the caller sleeps and restores it.
fn redraw_nudge(master: &dyn MasterPty) -> Option<PtySize> {
    // The size the pty actually has, read back rather than tracked: the daemon is
    // free to have resized since, and a remembered value would restore a stale one.
    let size = match master.get_size() {
        Ok(size) => size,
        Err(e) => {
            debug!("skipping a redraw: the terminal size could not be read: {e}");
            return None;
        }
    };
    // Down a row, not up: a program that draws one row short leaves the bottom
    // line stale for the length of the gap, while one that draws a row *past* the
    // screen scrolls the display and moves everything the user was looking at.
    //
    // The `else` is for a one-row terminal, which can only be nudged upwards. It is
    // deliberately not claimed to cover `rows == 0`: that is unreachable — the spawn
    // size and every `resize` run through `clamp_dimension`, which maps 0 to the
    // default — and it would not work if it were, because the restore below goes
    // through that same clamp and would set 24 rows rather than putting 0 back.
    let nudged = if size.rows > 1 {
        size.rows - 1
    } else {
        size.rows + 1
    };
    resize(master, size.cols, nudged);
    Some(size)
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

/// Whether a foreground job other than the shell itself is running.
///
/// `tcgetpgrp` on the master reports the terminal's *foreground* process group:
/// the shell's own pgrp while it sits at a prompt, the running job's pgrp while
/// a command executes. The shell is a session leader ([`setsid`] in
/// [`spawn_shell`]), so its pgrp equals its pid — a different value means a
/// foreground job. This is the same signal a real terminal uses to decide
/// whether closing would lose a running process.
///
/// Works from the master even though the holder is not the controlling
/// terminal: the foreground pgrp is a property of the tty, not of the caller.
fn session_busy(master: &dyn MasterPty, shell_pid: i32) -> bool {
    let Some(fd) = master.as_raw_fd() else {
        return false;
    };
    // -1 on error (no foreground group yet, or the fd is gone). Treat that as
    // idle: there is no job to warn about, and reporting busy here would block
    // a close on a terminal we cannot actually read.
    let fg = unsafe { libc::tcgetpgrp(fd) };
    fg >= 0 && fg != shell_pid
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
            shell_argv: None,
            argv: None,
            env: std::collections::BTreeMap::new(),
            pane_label: None,
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

    /// A config-declared pane runs its own command, with the `PATH` the daemon
    /// resolved rather than the one this process happens to have.
    ///
    /// The `PATH` half is the point. Every ordinary terminal in this module gets
    /// its `PATH` from the login shell it spawns, which is why `spawn_shell`
    /// documents itself as an exception to the AGENTS.md rule — and a pane now
    /// does too, because `resolve_pane` wraps the pane command in a login+interactive
    /// shell before it ever reaches the holder. But the **holder's** contract is
    /// unchanged: whatever `argv` it is handed it spawns directly and layers the
    /// injected `env` on top. This test pins that contract so the injected `PATH`
    /// (a floor for a shell with no rc files) and `VELD_PANE_*` actually reach
    /// the command. The command prints what it got, so a regression shows up as
    /// the wrong string rather than as a pane that works on the developer's
    /// machine and not from the app.
    #[tokio::test]
    async fn a_pane_command_runs_instead_of_a_shell_and_gets_the_injected_path() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("p.sock");
        let cfg = HolderConfig {
            session_id: "paneprobe".to_owned(),
            worktree_id: 1,
            label: "test".to_owned(),
            cwd: dir.path().to_path_buf(),
            cols: 80,
            rows: 24,
            socket: socket.clone(),
            orphan_grace_secs: 30,
            shell_argv: None,
            argv: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "printf 'PANE[%s][%s]' \"$PATH\" \"$VELD_PANE_TOKEN\"".to_owned(),
            ]),
            env: std::collections::BTreeMap::from([
                ("PATH".to_owned(), "/injected/bin:/usr/bin:/bin".to_owned()),
                ("VELD_PANE_TOKEN".to_owned(), "tok-123".to_owned()),
            ]),
            pane_label: Some("Probe".to_owned()),
        };
        tokio::spawn(run(cfg));

        let mut stream = loop {
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(s) => break s,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        // Collect until the command's own exit frame arrives, so the assertion
        // never races the pty read loop.
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "no exit frame; saw {seen:?}");
            let Some(frame) = wire::read_frame(&mut stream).await.unwrap() else {
                break;
            };
            match frame.kind {
                wire::OUTPUT | wire::SCROLLBACK => {
                    seen.extend_from_slice(&frame.payload);
                }
                wire::EXIT => break,
                _ => {}
            }
        }
        let text = String::from_utf8_lossy(&seen);
        assert!(
            text.contains("PANE[/injected/bin:/usr/bin:/bin][tok-123]"),
            "the pane command must run with the daemon's PATH and env, got: {text:?}"
        );
        // End to end, not just `exit_notice`'s own unit test: the label has to
        // survive stdin, serde and the frame it is written into, and this line
        // is often the only thing left on a pane once a full-screen program has
        // restored the primary buffer on its way out.
        assert!(
            text.contains("[veld] Probe exited (0)") && !text.contains("shell exited"),
            "the exit notice must name the pane, not claim a shell ran: {text:?}"
        );
    }

    /// [`wire::REDRAW`] gives the foreground program a real size *change*, and a
    /// same-size [`wire::RESIZE`] gives it nothing.
    ///
    /// Three properties, and each one is a bug that was actually built:
    ///
    /// 1. **A same-size resize signals nothing.** The obvious fix — resend the
    ///    size on reattach — does nothing, because Linux (`tty_do_resize`) and XNU
    ///    (`ttioctl_locked`) compare the new `winsize` with the old and skip the
    ///    `SIGWINCH` when they match. Asserting only the REDRAW half would leave
    ///    that premise untested, and it is the premise that makes the whole frame
    ///    necessary rather than redundant.
    /// 2. **REDRAW changes the size, it does not merely signal.** The first version
    ///    of this delivered a bare `SIGWINCH` via `killpg`, and that is not enough:
    ///    a renderer that diffs its own output recomputes the same frame from the
    ///    same size and writes nothing. So the assertion is on the *sizes the
    ///    program observed* — `rows - 1` and then `rows` — which a bare-signal
    ///    implementation cannot produce.
    /// 3. **And it puts the size back.** Nudging without restoring leaves every
    ///    reattached terminal one row short, which no assertion on "did it repaint"
    ///    would catch.
    ///
    /// The probe reports through `stty size` rather than a literal, so the trap
    /// body cannot satisfy the assertion by being echoed. It busy-loops instead of
    /// sleeping because a POSIX shell defers a trap until the foreground command it
    /// is waiting on returns, and a `sleep`-based loop would put a whole `sleep` of
    /// latency between signal and output — long enough for property 1 to pass by
    /// simply not having waited.
    #[tokio::test]
    async fn a_redraw_changes_the_size_and_restores_it_while_a_same_size_resize_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("w.sock");
        let cfg = HolderConfig {
            session_id: "winchprobe".to_owned(),
            worktree_id: 1,
            label: "test".to_owned(),
            cwd: dir.path().to_path_buf(),
            cols: 80,
            rows: 24,
            socket: socket.clone(),
            // A backstop: this probe holds the CPU, so it must never outlive the
            // test even if the explicit hangup below is lost.
            orphan_grace_secs: 5,
            shell_argv: None,
            // 28 is `SIGWINCH` on both Linux and Darwin; the numeric form is what
            // every POSIX shell accepts, `WINCH` is not.
            argv: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "trap 'stty size' 28; printf \"VELDR%sY\" EAD; while :; do :; done".to_owned(),
            ]),
            env: std::collections::BTreeMap::from([(
                "PATH".to_owned(),
                "/usr/bin:/bin".to_owned(),
            )]),
            pane_label: Some("Probe".to_owned()),
        };
        tokio::spawn(run(cfg));

        let mut stream = loop {
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(s) => break s,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };

        let mut seen = Vec::new();
        let mut pid = 0;
        // Read frames until `needle` shows up in the pty output.
        //
        // The bound is around the *read*, not only between reads: this probe prints
        // nothing unasked, so a missing signal means no frame ever arrives and a
        // deadline checked between whole frames is never reached — the first shape
        // of this helper turned a failure into a ten-minute hang. Cancelling a
        // `read_frame` mid-frame desyncs the stream, which is why the timeout is
        // fatal rather than retried.
        macro_rules! read_until {
            ($needle:expr) => {{
                let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                loop {
                    let text = String::from_utf8_lossy(&seen).to_string();
                    if text.contains($needle) {
                        break text;
                    }
                    let read = tokio::time::timeout_at(deadline, wire::read_frame(&mut stream));
                    let Ok(framed) = read.await else {
                        panic!("never saw {:?}; saw {text:?}", $needle);
                    };
                    let Some(frame) = framed.unwrap() else {
                        panic!("the holder closed before {:?}; saw {text:?}", $needle);
                    };
                    match frame.kind {
                        wire::OUTPUT | wire::SCROLLBACK => seen.extend_from_slice(&frame.payload),
                        wire::HELLO => {
                            pid = serde_json::from_slice::<wire::Hello>(&frame.payload)
                                .unwrap()
                                .pid;
                        }
                        _ => {}
                    }
                }
            }};
        }

        read_until!("VELDREADY");

        // Property 1: the size the holder already spawned with. Nothing comes back.
        wire::write_frame(&mut stream, wire::RESIZE, &wire::encode_size(80, 24))
            .await
            .unwrap();
        // Deliberately *not* reading during the wait, so nothing is cancelled: any
        // frame the resize produced queues in the socket and is read below, and the
        // pty is one ordered stream, so output provoked here cannot arrive after the
        // echo of a marker sent after it.
        tokio::time::sleep(REDRAW_NUDGE * 5).await;
        // The line discipline echoes input whether or not the child reads it, which
        // is what makes the marker observable against a probe that never reads stdin.
        wire::write_frame(&mut stream, wire::INPUT, b"MARK\r")
            .await
            .unwrap();
        let text = read_until!("MARK");
        assert!(
            !text.contains("24 80") && !text.contains("23 80"),
            "a resize to the size the pty already has must not signal anything — if \
             this starts passing trivially, the frame under test is unnecessary: {text:?}"
        );

        // Properties 2 and 3: a real change, then back.
        wire::write_frame(&mut stream, wire::REDRAW, &[])
            .await
            .unwrap();
        let text = read_until!("24 80");
        let nudged = text.find("23 80").expect(
            "REDRAW must give the program a size it has not already rendered at — a bare \
             SIGWINCH at the unchanged size leaves a diffing renderer writing nothing",
        );
        assert!(
            nudged < text.find("24 80").unwrap(),
            "the nudge must come first and the true size last, or every reattached \
             terminal is left a row short: {text:?}"
        );

        // The probe holds a core; end it rather than leaving it to the grace.
        wire::write_frame(&mut stream, wire::HANGUP, &[])
            .await
            .unwrap();
        assert!(pid > 0, "the greeting must have carried the probe's pid");
        let deadline = Instant::now() + Duration::from_secs(20);
        // SAFETY: signal 0 performs the permission/existence check only.
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(Instant::now() < deadline, "the probe outlived its hangup");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// An ordinary terminal opens the shell the **daemon** chose, not this
    /// process's `$SHELL`.
    ///
    /// The whole `terminal.shell` preference rests on this hop: the setting lives
    /// in the database, the holder has none, so the value travels in
    /// [`HolderConfig::shell_argv`] and a holder that ignored it would leave the picker
    /// changing nothing while every unit test still passed. Asserted with a stub
    /// shell that prints its own `argv`, which also pins the `-l` — a bash spawned
    /// without it reads no `~/.bash_profile`, i.e. none of the startup files the
    /// setting exists to load.
    #[tokio::test]
    async fn the_daemon_chosen_shell_is_what_a_terminal_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("stub-shell");
        std::fs::write(&stub, "#!/bin/sh\nprintf 'ARGV[%s][%s]' \"$0\" \"$1\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let socket = dir.path().join("sh.sock");
        let cfg = HolderConfig {
            session_id: "shellprobe".to_owned(),
            worktree_id: 1,
            label: "test".to_owned(),
            cwd: dir.path().to_path_buf(),
            cols: 80,
            rows: 24,
            socket: socket.clone(),
            orphan_grace_secs: 30,
            shell_argv: Some(vec![stub.display().to_string(), "-l".to_owned()]),
            // No argv: this is the ordinary-terminal path, the one the preference
            // governs.
            argv: None,
            env: std::collections::BTreeMap::new(),
            pane_label: None,
        };
        tokio::spawn(run(cfg));

        let mut stream = loop {
            match tokio::net::UnixStream::connect(&socket).await {
                Ok(s) => break s,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "no exit frame; saw {seen:?}");
            let Some(frame) = wire::read_frame(&mut stream).await.unwrap() else {
                break;
            };
            match frame.kind {
                wire::OUTPUT | wire::SCROLLBACK => seen.extend_from_slice(&frame.payload),
                wire::EXIT => break,
                _ => {}
            }
        }
        let text = String::from_utf8_lossy(&seen);
        assert!(
            text.contains(&format!("ARGV[{}][-l]", stub.display())),
            "the holder must spawn the daemon's shell as a login shell, got: {text:?}"
        );
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
            shell_argv: None,
            argv: None,
            env: std::collections::BTreeMap::new(),
            pane_label: None,
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

    /// Build a holder for a session that echoes what is typed into it, and
    /// return its socket path once it is listening.
    ///
    /// `cat` rather than a login shell: the assertions below are about bytes
    /// coming back out of the PTY, and a developer's `.zshrc` is not a fixture.
    async fn echo_holder(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let socket = dir.join(format!("{name}.sock"));
        let cfg = HolderConfig {
            session_id: name.to_owned(),
            worktree_id: 1,
            label: "test".to_owned(),
            cwd: dir.to_path_buf(),
            cols: 80,
            rows: 24,
            socket: socket.clone(),
            // Long, so nothing in these tests can be explained by the orphan path.
            orphan_grace_secs: 3600,
            shell_argv: None,
            argv: Some(vec!["cat".to_owned()]),
            env: std::collections::BTreeMap::new(),
            pane_label: None,
        };
        tokio::spawn(run(cfg));
        socket
    }

    /// Connect, and read frames until the greeting is complete.
    async fn greet(socket: &std::path::Path) -> tokio::net::UnixStream {
        let mut stream = loop {
            match tokio::net::UnixStream::connect(socket).await {
                Ok(s) => break s,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        for want in [wire::HELLO, wire::SCROLLBACK] {
            let frame = wire::read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(frame.kind, want);
        }
        stream
    }

    /// Type `marker` and wait for the PTY to echo it back, or say what arrived
    /// instead.
    async fn echoes_back(stream: &mut tokio::net::UnixStream, marker: &str) -> bool {
        wire::write_frame(stream, wire::INPUT, format!("{marker}\n").as_bytes())
            .await
            .unwrap();
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), wire::read_frame(stream)).await {
                // The connection ended: whatever this is, it is not an echo.
                Ok(Ok(None)) | Ok(Err(_)) => return false,
                Ok(Ok(Some(frame))) => {
                    if frame.kind == wire::OUTPUT {
                        seen.extend_from_slice(&frame.payload);
                        if String::from_utf8_lossy(&seen).contains(marker) {
                            return true;
                        }
                    }
                }
                Err(_) => {}
            }
        }
        false
    }

    /// A bare connect — the shape of every liveness probe there is — must not
    /// take the session away from the daemon that has it.
    ///
    /// This is the whole bug. `veld doctor` walks the holder directory and
    /// connects to each socket to count the live ones; `bind` connects to a path
    /// already in use to tell a stale file from a running holder, which happens on
    /// every resume of a session whose holder outlived its daemon link. Both close
    /// immediately. While an accepted connection displaced the attached daemon on
    /// the spot, both severed live sessions: the daemon read EOF, published an exit
    /// for a shell that was still running, and the reaper hung that shell up half
    /// an hour later. One `veld doctor` did it to every terminal on the machine.
    #[tokio::test]
    async fn a_probe_connection_does_not_take_a_terminal_from_its_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let socket = echo_holder(dir.path(), "probeprobe").await;
        let mut daemon = greet(&socket).await;
        assert!(
            echoes_back(&mut daemon, "BEFORE-PROBE").await,
            "the fixture must be working before the probe"
        );

        // Three of them, because doctor probes every socket it finds and a resume
        // probes on every retry.
        for _ in 0..3 {
            let probe = std::os::unix::net::UnixStream::connect(&socket).unwrap();
            drop(probe);
        }

        assert!(
            echoes_back(&mut daemon, "AFTER-PROBE").await,
            "a connect-and-close must leave the attached daemon's connection alone"
        );
    }

    /// A peer that connects, says nothing, and simply stays gets the session once
    /// the window has passed — and gets the output it missed while waiting.
    ///
    /// The timer is the only promotion path a *silent* peer has, and a daemon
    /// adopting a session with no client attached is exactly that: nothing makes
    /// it send `INPUT` or `RESIZE` until somebody opens the pane. Without the
    /// timer such a peer would wait behind an incumbent forever. The scrollback
    /// half is the other half of the same promise: its greeting snapshot was taken
    /// before `DURING-PROBATION` was printed, so the bytes can only reach it if
    /// they were teed to it while it waited.
    #[tokio::test]
    async fn a_silent_peer_takes_over_after_the_probation_and_misses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let socket = echo_holder(dir.path(), "silentprobe").await;
        let mut first = greet(&socket).await;
        assert!(echoes_back(&mut first, "FIRST").await);

        // Connect and say nothing at all.
        let mut second = greet(&socket).await;
        // Printed while the newcomer is still on probation, so it is in neither
        // its greeting snapshot nor anything it could ask for later.
        assert!(echoes_back(&mut first, "DURING-PROBATION").await);

        // Teed to the waiting peer, so it has the bytes before it owns anything.
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains("DURING-PROBATION") {
            assert!(
                Instant::now() < deadline,
                "a probationary peer must be fed the shell's output; saw {:?}",
                String::from_utf8_lossy(&seen)
            );
            match tokio::time::timeout(Duration::from_secs(1), wire::read_frame(&mut second)).await
            {
                Ok(Ok(Some(frame))) if frame.kind == wire::OUTPUT => {
                    seen.extend_from_slice(&frame.payload)
                }
                Ok(Ok(Some(_))) => {}
                Ok(Ok(None)) | Ok(Err(_)) => panic!("the probationary connection was dropped"),
                Err(_) => {}
            }
        }

        // Now the timer, and *only* the timer: nothing is written on the second
        // connection, so if it is promoted at all it is because it stayed. The
        // observable side of a promotion is the incumbent being dropped.
        let promoted = tokio::time::timeout(TAKEOVER_PROBATION + Duration::from_secs(20), async {
            while let Ok(Some(_)) = wire::read_frame(&mut first).await {}
        })
        .await;
        assert!(
            promoted.is_ok(),
            "a peer that connected and stayed silent must still take the session over"
        );
        // And it owns the session now: its input reaches the PTY.
        assert!(echoes_back(&mut second, "AFTER-PROBATION").await);
    }

    /// The other half of the same rule: a peer that *speaks* is a peer, and gets
    /// the session at once rather than serving the window out — a frame only a
    /// daemon sends is stronger evidence than time is. Deleting the probation
    /// entirely would pass the probe test above trivially, by never handing a
    /// session over at all; this is what stops that.
    #[tokio::test]
    async fn a_peer_that_speaks_takes_the_session_over_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let socket = echo_holder(dir.path(), "takeoverprobe").await;
        let mut first = greet(&socket).await;
        assert!(echoes_back(&mut first, "FIRST").await);

        let mut second = greet(&socket).await;
        assert!(
            echoes_back(&mut second, "SECOND").await,
            "a peer that holds its connection open must get the session"
        );
        // And the displaced one is told, by the only means the protocol has: its
        // connection ends.
        let closed = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match wire::read_frame(&mut first).await {
                    Ok(Some(_)) => continue,
                    _ => return,
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "the displaced connection must be dropped");
    }

    /// A peer whose **only** frame is a `REDRAW` gets the session at once, the same
    /// as one that sends `INPUT` or `RESIZE`.
    ///
    /// This is the promotion list at the top of the `Cmd::Frame` arm, and it needs a
    /// test of its own because the code works without it. The daemon sends a
    /// same-size `RESIZE` immediately before every `REDRAW` (`resize_session` then
    /// `redraw_session` in `serve_socket`), and that `RESIZE` promotes the peer, so
    /// deleting `REDRAW` from the promotion list leaves every other test in this
    /// file green while the repaint is silently dropped on exactly the adoption
    /// race the comment there claims to cover. Worse, the resize it depends on is
    /// one this module's own docs argue is useless — a same-size resize signals
    /// nothing — so the natural cleanup is what would break it.
    ///
    /// **The vacuity trap:** a silent peer is promoted anyway once
    /// [`TAKEOVER_PROBATION`] elapses, so "the incumbent was eventually dropped"
    /// would pass with no promotion arm at all. The assertion is therefore that the
    /// handover beats that timer — promotion by frame is a channel send and a loop
    /// turn, i.e. milliseconds, while the timer cannot fire before a full second.
    #[tokio::test]
    async fn a_peer_whose_only_frame_is_a_redraw_takes_the_session_over_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let socket = echo_holder(dir.path(), "redrawtakeover").await;
        let mut first = greet(&socket).await;
        assert!(echoes_back(&mut first, "FIRST").await);

        let mut second = greet(&socket).await;
        // The only frame this peer ever sends. No `RESIZE` ahead of it, which is
        // what makes the promotion attributable to `REDRAW` alone.
        wire::write_frame(&mut second, wire::REDRAW, &[])
            .await
            .unwrap();

        // The displaced connection ending is the only signal the protocol has for a
        // completed handover. Bounded well under the probation so the timer cannot
        // be what produced it.
        let closed = tokio::time::timeout(TAKEOVER_PROBATION * 4 / 5, async {
            while let Ok(Some(_)) = wire::read_frame(&mut first).await {}
        })
        .await;
        assert!(
            closed.is_ok(),
            "a REDRAW must promote a probationary peer by itself — waiting for the \
             probation timer instead means the repaint is dropped on every adoption"
        );
        // And it really owns the session: its input reaches the PTY.
        assert!(echoes_back(&mut second, "OWNS-IT").await);
    }

    #[test]
    fn the_exit_notice_is_a_complete_line() {
        let notice = String::from_utf8(exit_notice(7, None)).unwrap();
        // A notice without the leading CRLF lands on top of whatever the shell
        // last printed, and one without the trailing pair leaves the client's
        // cursor mid-line.
        assert!(notice.starts_with("\r\n"));
        assert!(notice.ends_with("\r\n"));
        assert!(notice.contains("shell exited (7)"));

        // A config-declared pane never ran a shell, and after a full-screen
        // program restores the primary buffer this line is often the only thing
        // left on the pane — so calling it a shell is the one visible claim the
        // pane makes about itself, and it would be false.
        let named = String::from_utf8(exit_notice(0, Some("Claude"))).unwrap();
        assert!(named.contains("Claude exited (0)"), "{named:?}");
        assert!(!named.contains("shell"), "{named:?}");
    }

    /// Create a socket *file* at `path` that has no listener and never will.
    ///
    /// A killed holder's socket file is "stale" when nothing is left listening on
    /// it, and this is how the stale case is made: a socket bound but never
    /// `listen`ed. The *bound-but-not-listening* shape is what matters, not that a
    /// listener happened to be dropped:
    ///
    /// - Bind a real listener and drop it, and the file is stale — *usually*.
    ///   But in the shared test binary another test's `fork` can land in the
    ///   window while the listener is still open, inherit its fd, and keep the
    ///   path connectable for that child's whole lifetime — a child holding an
    ///   inherited *listening* fd makes `connect` succeed, which is precisely the
    ///   [`bind`] refusal this test is about. That flaked the full binary
    ///   intermittently (and never in isolation), because it depends on a race
    ///   with every other test's forks.
    /// - A socket that was bound but never `listen`ed answers *no* connection,
    ///   inherited fd or not, so the stale file is deterministic here.
    ///
    /// From [`bind`]'s probe — `connect` must fail for the file to count as
    /// stale — the two shapes are indistinguishable, so this exercises the same
    /// code path without the fork race.
    fn stale_socket(path: &std::path::Path) {
        use std::os::unix::ffi::OsStrExt;
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .expect("the test path has no interior NUL");
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(
            fd >= 0,
            "socket() failed: {}",
            std::io::Error::last_os_error()
        );
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = c_path.as_bytes();
        assert!(
            bytes.len() < std::mem::size_of_val(&addr.sun_path),
            "test socket path too long: {}",
            path.display()
        );
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                addr.sun_path.as_mut_ptr() as *mut u8,
                bytes.len(),
            );
        }
        #[cfg(target_os = "macos")]
        {
            addr.sun_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
                as libc::c_uchar;
        }
        let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
            as libc::socklen_t;
        let rc = unsafe {
            libc::bind(
                fd,
                &addr as *const libc::sockaddr_un as *const libc::sockaddr,
                len,
            )
        };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            unsafe { libc::close(fd) };
            panic!("bind() failed on {}: {e}", path.display());
        }
        unsafe { libc::close(fd) };
        assert!(path.exists(), "binding must leave a socket file behind");
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
        stale_socket(&path);
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
