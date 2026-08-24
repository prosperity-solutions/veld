//! Framing for the daemon ↔ holder unix socket.
//!
//! One frame is `[kind: u8][len: u32 big-endian][payload: len bytes]`. Both
//! directions use the same encoding; the kinds are disjoint so a frame read on
//! the wrong side is a protocol error rather than a plausible-looking command.
//!
//! # Versioning is load-bearing here
//!
//! A holder outlives the daemon that spawned it — that is the entire point — so
//! after `veld update` the daemon is the *new* binary while every holder still
//! runs the old one. The two therefore negotiate: the holder's first frame is a
//! [`Hello`] carrying [`PROTOCOL`], and a daemon that does not recognise the
//! version must treat the session as gone rather than guess at the framing.
//!
//! That leaves one problem — a session the daemon refuses to speak to still has
//! a live shell, and abandoning it would leak a process no future daemon can
//! reach either (every daemon would make the same refusal). So exactly one
//! frame is **stable across all protocol versions, forever**: [`HANGUP`], with
//! an empty payload. Any daemon can always tell any holder to end its shell and
//! exit. Never reuse the byte `0x83` for anything else, and never give it a
//! payload that matters — a holder from a future version must be able to obey it
//! without understanding anything else on the wire.
//!
//! Concretely, the contract a hangup-only caller relies on is: **connect, write
//! the five bytes, and you may close immediately.** The holder spawns its frame
//! reader before it writes its own greeting precisely so that this works — three
//! callers depend on it (`veld uninstall`'s sweep, the recovery test's cleanup
//! guard, and the daemon refusing an unrecognised version), and greeting-first
//! made all three silently ineffective: the peer had already gone, the greeting
//! failed EPIPE, and the hangup was never read while the shell kept running.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The framing/semantics version spoken by this build.
///
/// Bump it when a change would make an older peer misread the wire — a new
/// frame kind that the other side must understand to stay correct, a changed
/// payload layout, or a changed meaning for an existing kind. Adding a frame
/// kind that a peer can safely *ignore* does not need a bump; see
/// [`Frame::is_ignorable`].
///
/// **A bump costs every open terminal, once.** The daemon compares with `!=`, so
/// the update that ships a new version meets holders speaking the old one,
/// refuses them and hangs their shells up — the pre-holder behaviour, for exactly
/// that one update. That is the deliberate trade (a wrong guess about the framing
/// is worse than a lost shell), so bumping this is a user-visible decision, not
/// housekeeping. If a change ever has to preserve sessions across it, add a
/// `MIN_SUPPORTED` and keep the old decode path rather than widening the
/// comparison.
///
/// **The exception:** a new JSON field carrying `#[serde(default)]` costs nothing in
/// either direction, because serde ignores unknown fields and defaults missing
/// ones. So *optional additive* fields are free; renames, removals and any field
/// without a default are not — a missing required field fails deserialization,
/// which is not even reported as a version problem (the daemon cannot read the
/// greeting at all, so it cannot tell what version sent it). Either add the
/// default or bump this. Both [`Hello`]'s `detached_secs` and [`HolderConfig`]'s
/// `env` were added under that exception.
///
/// **[`HolderConfig`] is not governed by this constant at all**, which is worth
/// knowing before adding a field to it: the daemon writes that struct to the
/// holder's stdin *before* any [`Hello`] exists to carry a version, and it writes it
/// with the binary it spawned from `current_exe()`. There is no negotiation to
/// bump — its compatibility rests on `#[serde(default)]` alone, for the one case
/// that can still bite: a holder from an older release, adopted by this daemon,
/// whose config was written by that older binary.
pub const PROTOCOL: u32 = 1;

// Holder → daemon.
/// Session metadata, JSON. Always the first frame on a connection.
pub const HELLO: u8 = 0x01;
/// The holder's scrollback ring, raw PTY bytes. Sent once, after [`HELLO`].
pub const SCROLLBACK: u8 = 0x02;
/// Live PTY output, raw bytes.
pub const OUTPUT: u8 = 0x03;
/// The shell exited; payload is its status as `u32` big-endian.
pub const EXIT: u8 = 0x04;
/// The holder's answer to [`QUERY_BUSY`]: a single byte, `0` = idle, `1` =
/// busy. Holder-numbered, so an older daemon that does not know it treats it as
/// ignorable ([`Frame::is_ignorable`]) and nothing breaks.
pub const BUSY: u8 = 0x05;

// Daemon → holder.
/// Keystrokes for the PTY, raw bytes.
pub const INPUT: u8 = 0x81;
/// Window size; payload is `cols` then `rows`, each `u16` big-endian.
pub const RESIZE: u8 = 0x82;
/// End the shell and exit. **Stable across every protocol version** — see the
/// module docs before touching this.
pub const HANGUP: u8 = 0x83;
/// Ask the holder whether a foreground job is running; it answers with
/// [`BUSY`]. Sent only to a holder that advertised [`Hello::supports_busy`] —
/// an older holder that does not know the kind would drop the connection
/// rather than ignore it, which is the one outcome that must never happen.
pub const QUERY_BUSY: u8 = 0x84;
/// Make the foreground program repaint, by giving it a real `winsize` change —
/// one row shorter, a gap, then the true size — so the *kernel* signals the PTY's
/// foreground process group.
///
/// Empty payload — the size is not part of this frame, deliberately. The holder
/// reads the pty's current size itself, because the daemon asks for a repaint
/// precisely when the size has *not* changed, which is the case [`RESIZE`] cannot
/// cover: both Linux (`tty_do_resize`) and XNU (`ttioctl_locked`) skip the
/// `SIGWINCH` when the new `winsize` equals the old one, so re-sending the size a
/// shell already has reaches nothing. A bare signal is not enough either — a
/// renderer that diffs its own output recomputes an identical frame and writes
/// nothing — which is why this is a change and not a notification. See
/// `redraw_nudge` in the holder.
///
/// Sent only to a holder that advertised [`Hello::supports_redraw`], for the
/// same reason [`QUERY_BUSY`] is gated: an older holder drops the connection on
/// a daemon-numbered frame it does not know ([`Frame::is_ignorable`]).
pub const REDRAW: u8 = 0x85;

/// Cap on one frame's payload, matching the WebSocket frame cap the daemon
/// applies on the other side of the bridge. The largest legitimate frame is a
/// full scrollback snapshot, which is a quarter of this.
pub const MAX_PAYLOAD: usize = 1024 * 1024;

/// A decoded frame. The payload is raw bytes; interpreting it is the caller's
/// job, keyed on `kind`.
pub struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

impl std::fmt::Debug for Frame {
    /// Deliberately not the payload: it is terminal output, which means escape
    /// sequences, a quarter of a megabyte of scrollback, and — for an input
    /// frame — whatever the user just typed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("kind", &format_args!("{:#x}", self.kind))
            .field("bytes", &self.payload.len())
            .finish()
    }
}

impl Frame {
    /// Whether an unrecognised frame can be skipped instead of failing the
    /// connection.
    ///
    /// Only frames the *peer* sends are ignorable, and only in the direction
    /// that cannot change state: a holder that receives an unknown
    /// daemon-numbered frame (`0x80`-and-up) has been sent an instruction it
    /// cannot carry out, and pretending otherwise is how a resize silently
    /// stops working. An unknown holder-numbered frame reaching the daemon is
    /// data it did not ask for, which it can drop.
    pub fn is_ignorable(&self) -> bool {
        self.kind < 0x80
    }
}

/// What a holder tells a connecting daemon about itself.
///
/// Everything here was fixed when the holder was spawned, except `exited` —
/// which is what lets a daemon adopt a session whose shell has already finished
/// and report the exit instead of presenting a dead prompt as live.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub session_id: String,
    pub worktree_id: i64,
    /// Worktree alias, for log lines.
    pub label: String,
    /// The shell's working directory at spawn.
    pub cwd: String,
    /// The shell's pid, for log lines. The daemon never signals it — the holder
    /// owns every signal, because it owns the unreaped child.
    pub pid: i32,
    /// Whether this holder answers [`QUERY_BUSY`] with a [`BUSY`] reply.
    ///
    /// `#[serde(default)]`, so this is additive and needs no [`PROTOCOL`] bump
    /// (see its docs). A daemon never sends `QUERY_BUSY` to a holder that does
    /// not advertise it — an old holder that cannot answer would drop the
    /// connection rather than ignore the frame — so an old holder adopted after
    /// an update defaults to false and its sessions simply report as idle.
    #[serde(default)]
    pub supports_busy: bool,
    /// Whether this holder acts on [`REDRAW`].
    ///
    /// `#[serde(default)]` for the same reason [`Self::supports_busy`] is, and
    /// gated for the same reason: an older holder adopted after an update would
    /// drop the connection rather than ignore the frame. Defaulting to false
    /// costs such a session nothing but the repaint — it reattaches exactly as
    /// it did before this existed.
    #[serde(default)]
    pub supports_redraw: bool,
    /// `Some(code)` if the shell has already exited.
    pub exited: Option<u32>,
    /// Seconds since a daemon was last connected, or `None` if one is attached
    /// right now — which means a **takeover**, not a fresh spawn: a newly started
    /// holder has never had a daemon, so its clock is already running and it
    /// reports `Some(~0)`.
    ///
    /// Without this, adoption restarted the detach clock at zero on every daemon
    /// start: the 30-minute bound on "a shell nobody will ever come back to"
    /// (`DETACH_GRACE`) then never elapsed for a daemon that restarts more often
    /// than that — a crash-looping one under `Restart=always` kept abandoned
    /// shells alive indefinitely. The daemon seeds its own clock from this so the
    /// grace measures what it claims to measure: time since anyone looked.
    #[serde(default)]
    pub detached_secs: Option<u64>,
}

/// The holder's whole configuration, handed to it on stdin as one JSON line.
///
/// On stdin rather than in `argv` because the process table is world-readable
/// and this carries a filesystem path plus a worktree alias; nothing here is a
/// secret, but the repo's rule is that a command line is not where values go.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HolderConfig {
    pub session_id: String,
    pub worktree_id: i64,
    pub label: String,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    /// Where to listen. Chosen by the daemon so both sides derive it once.
    pub socket: PathBuf,
    /// How long to keep a shell with no daemon connected. Passed in rather than
    /// compiled in so the two sides cannot disagree about it, and so a test can
    /// shorten it.
    pub orphan_grace_secs: u64,
    /// Which shell an ordinary terminal opens — `terminal.shell`, already
    /// resolved by the daemon (`veld_core::shell::resolve`, via
    /// `Db::terminal_shell`).
    ///
    /// Resolved by the daemon and sent, rather than read by the holder, for the
    /// reason [`Self::env`] is: the holder is a dumb PTY owner with no database.
    /// It is also what keeps the two sides from disagreeing — the daemon has
    /// already decided, from this same value, whether the session's environment
    /// carries the zsh-only `ZDOTDIR` handoff.
    ///
    /// The **full argv** — program and flags — rather than just the path, because
    /// the flags are not fixed any more: bash gets a leading `--posix` when the
    /// daemon put an `$ENV` handoff in [`Self::env`] (see `pty::shims`), and bash
    /// parses GNU long options only ahead of the short ones, so the order is the
    /// daemon's to get right in one place rather than the holder's to reconstruct.
    ///
    /// `#[serde(default)]`, so this is additive and needs no [`PROTOCOL`] bump
    /// (see its docs): a holder started by an older daemon has no entry and falls
    /// back to `<its own $SHELL> -l`, which is what that daemon did anyway. Ignored
    /// entirely when [`Self::argv`] is set — a pane's command has the shell
    /// wrapped around it already.
    #[serde(default)]
    pub shell_argv: Option<Vec<String>>,
    /// What to run instead of the user's login shell — a config-declared pane's
    /// command, already resolved and interpolated by the daemon.
    ///
    /// `None` is the ordinary terminal: `<shell> -l`. Additive with a default, so
    /// this did not bump [`PROTOCOL`] (see its docs) — an old holder adopted by a
    /// new daemon simply keeps running the shell it already started, which is the
    /// correct outcome, since the command only ever matters at spawn time.
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    /// Extra environment for the session's process, on top of what the holder
    /// inherits.
    ///
    /// The daemon computes it (`pty::shims::session_env`) because it is the side
    /// that knows about instances, ports and where the `veld` CLI lives; the holder
    /// only applies it.
    ///
    /// It is also how a **pane command** gets a usable `PATH`. `resolve_pane`
    /// wraps the pane's command in the user's login+interactive shell, which
    /// computes `PATH` itself, so this entry is a *floor* for a shell with no rc
    /// files — without it that shell would inherit the daemon's bare service
    /// `PATH` and fail to find every user-installed CLI a pane exists to run.
    ///
    /// `#[serde(default)]`, which is what keeps this off [`PROTOCOL`]: a holder
    /// spawned by an older daemon simply has no entry, and its shell has no
    /// `$BROWSER` — the pre-feature behaviour, for that one already-running shell.
    /// See the note on [`PROTOCOL`] about additive fields.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// What to call [`Self::argv`] when reporting that it exited.
    ///
    /// `None` is the ordinary terminal, which is a shell and says so. A pane
    /// that ran `claude` must not report that "the shell exited" — it did not
    /// run one, and the notice is the only thing left on screen once a
    /// full-screen program has restored the primary buffer on its way out.
    #[serde(default)]
    pub pane_label: Option<String>,
}

/// Write one frame.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    kind: u8,
    payload: &[u8],
) -> std::io::Result<()> {
    if payload.len() > MAX_PAYLOAD {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "frame payload too large",
        ));
    }
    // One buffer, one write: two writes could interleave with another task's
    // frame on the same socket and produce a header claiming somebody else's
    // payload.
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf).await?;
    w.flush().await
}

/// Read one frame. `Ok(None)` is a clean end of stream.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head).await {
        Ok(_) => {}
        // A peer that closed between frames is not an error. One that closed
        // *mid-header* is, but it is the same "the other side is gone" outcome
        // for every caller, so both land here.
        Err(e)
            if e.kind() == std::io::ErrorKind::UnexpectedEof
                || e.kind() == std::io::ErrorKind::ConnectionReset =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    }
    let kind = head[0];
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_PAYLOAD {
        // Refuse to allocate on a length this side never legitimately sends.
        // Without this a corrupt or hostile header is a multi-gigabyte
        // allocation.
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame payload of {len} bytes exceeds the {MAX_PAYLOAD}-byte cap"),
        ));
    }
    let mut payload = vec![0u8; len];
    match r.read_exact(&mut payload).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    Ok(Some(Frame { kind, payload }))
}

/// Encode a resize payload.
pub fn encode_size(cols: u16, rows: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[..2].copy_from_slice(&cols.to_be_bytes());
    out[2..].copy_from_slice(&rows.to_be_bytes());
    out
}

/// Decode a resize payload, rejecting a short one rather than reading a
/// dimension out of whatever happened to be adjacent.
pub fn decode_size(payload: &[u8]) -> Option<(u16, u16)> {
    if payload.len() != 4 {
        return None;
    }
    Some((
        u16::from_be_bytes([payload[0], payload[1]]),
        u16::from_be_bytes([payload[2], payload[3]]),
    ))
}

/// Encode a busy answer. `true` = a foreground job is running.
pub fn encode_busy(busy: bool) -> [u8; 1] {
    [u8::from(busy)]
}

/// Decode a busy answer, rejecting a payload that is not a single byte.
pub fn decode_busy(payload: &[u8]) -> Option<bool> {
    match payload {
        [0] => Some(false),
        [1] => Some(true),
        _ => None,
    }
}

/// Decode an exit payload.
pub fn decode_exit(payload: &[u8]) -> Option<u32> {
    if payload.len() != 4 {
        return None;
    }
    Some(u32::from_be_bytes([
        payload[0], payload[1], payload[2], payload[3],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_in_order() {
        let mut buf = Vec::new();
        write_frame(&mut buf, OUTPUT, b"hello").await.unwrap();
        write_frame(&mut buf, EXIT, &7u32.to_be_bytes())
            .await
            .unwrap();
        // An empty payload is legal and is what HANGUP always carries.
        write_frame(&mut buf, HANGUP, b"").await.unwrap();

        let mut r = buf.as_slice();
        let f = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!(f.kind, OUTPUT);
        assert_eq!(f.payload, b"hello");
        let f = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!(decode_exit(&f.payload), Some(7));
        let f = read_frame(&mut r).await.unwrap().unwrap();
        assert_eq!(f.kind, HANGUP);
        assert!(f.payload.is_empty());
        // Clean end of stream, not an error.
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_binary_payload_survives_framing() {
        // PTY output is arbitrary bytes: escape sequences, invalid UTF-8 from a
        // `cat` of a binary file, and NULs. Nothing may be re-encoded.
        let payload: Vec<u8> = (0u8..=255).chain(0u8..=255).collect();
        let mut buf = Vec::new();
        write_frame(&mut buf, OUTPUT, &payload).await.unwrap();
        let mut r = buf.as_slice();
        assert_eq!(read_frame(&mut r).await.unwrap().unwrap().payload, payload);
    }

    #[tokio::test]
    async fn an_oversized_length_is_refused_without_allocating() {
        // A header claiming 4 GiB must not become a 4 GiB allocation.
        let mut buf = vec![OUTPUT];
        buf.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut r = buf.as_slice();
        let e = read_frame(&mut r).await.expect_err("must refuse");
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn an_oversized_write_is_refused() {
        let mut buf = Vec::new();
        let e = write_frame(&mut buf, OUTPUT, &vec![0u8; MAX_PAYLOAD + 1])
            .await
            .expect_err("must refuse");
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(buf.is_empty(), "nothing may reach the wire");
    }

    #[tokio::test]
    async fn a_truncated_payload_reads_as_end_of_stream() {
        let mut buf = vec![OUTPUT];
        buf.extend_from_slice(&64u32.to_be_bytes());
        buf.extend_from_slice(b"only a few bytes");
        let mut r = buf.as_slice();
        assert!(read_frame(&mut r).await.unwrap().is_none());
    }

    #[test]
    fn sizes_round_trip_and_short_payloads_are_rejected() {
        assert_eq!(decode_size(&encode_size(132, 50)), Some((132, 50)));
        assert_eq!(decode_size(&encode_size(0, 0)), Some((0, 0)));
        // Reading a dimension out of a short payload would silently resize to
        // whatever was adjacent.
        assert_eq!(decode_size(&[0, 1, 2]), None);
        assert_eq!(decode_size(&[0, 1, 2, 3, 4]), None);
        assert_eq!(decode_exit(&[]), None);
        assert_eq!(decode_exit(&7u32.to_be_bytes()), Some(7));
    }

    #[test]
    fn only_holder_numbered_frames_are_ignorable() {
        // An instruction the holder cannot carry out must fail the connection,
        // not be skipped: a silently-dropped resize is a terminal stuck at the
        // wrong size with nothing in any log.
        for kind in [INPUT, RESIZE, HANGUP, QUERY_BUSY, REDRAW, 0x99] {
            assert!(
                !Frame {
                    kind,
                    payload: vec![]
                }
                .is_ignorable(),
                "{kind:#x} must not be ignorable"
            );
        }
        for kind in [HELLO, SCROLLBACK, OUTPUT, EXIT, BUSY, 0x7f] {
            assert!(
                Frame {
                    kind,
                    payload: vec![]
                }
                .is_ignorable(),
                "{kind:#x} should be ignorable"
            );
        }
    }

    #[test]
    fn every_frame_kind_has_its_own_byte() {
        // Reusing a byte compiles and passes every other test, while putting one
        // side's frame into the other's state machine. The module doc's "never
        // reuse 0x83" was comment-only until this test.
        let kinds = [
            ("HELLO", HELLO),
            ("SCROLLBACK", SCROLLBACK),
            ("OUTPUT", OUTPUT),
            ("EXIT", EXIT),
            ("BUSY", BUSY),
            ("INPUT", INPUT),
            ("RESIZE", RESIZE),
            ("HANGUP", HANGUP),
            ("QUERY_BUSY", QUERY_BUSY),
            ("REDRAW", REDRAW),
        ];
        let unique: std::collections::HashSet<u8> = kinds.iter().map(|(_, k)| *k).collect();
        assert_eq!(
            unique.len(),
            kinds.len(),
            "two frame kinds share a byte: {kinds:?}"
        );
        // And the direction split `is_ignorable` keys on must hold: holder-sent
        // kinds below 0x80, daemon-sent kinds at or above it.
        for (name, kind) in kinds {
            let holder_sent = matches!(name, "HELLO" | "SCROLLBACK" | "OUTPUT" | "EXIT" | "BUSY");
            assert_eq!(
                kind < 0x80,
                holder_sent,
                "{name} is on the wrong side of the 0x80 split"
            );
        }
    }

    #[test]
    fn hangup_is_pinned_to_its_byte() {
        // The one frame every version must understand. Changing this number
        // strands shells behind daemons that cannot ask them to stop.
        assert_eq!(HANGUP, 0x83);
        assert_eq!(PROTOCOL, 1);
    }

    /// A greeting from a holder that predates a capability flag must default it
    /// to false, not fail to parse.
    ///
    /// This is the `veld update` path, and it is the whole reason both flags
    /// exist: after an update the daemon is the new binary while every holder
    /// still runs the old one, so the *old* greeting is what the *new* daemon
    /// reads. A field without `#[serde(default)]` makes that greeting
    /// undeserializable — and the daemon cannot even report it as a version
    /// problem, because it never got as far as reading the version. It would
    /// hang up every surviving terminal instead.
    ///
    /// Both flags are asserted rather than only the new one: the failure is a
    /// property of the struct, so the test has to be the thing that notices the
    /// next field added without a default.
    #[test]
    fn a_greeting_without_the_capability_flags_still_parses() {
        // Exactly the fields an older holder sends, and nothing this build added.
        let old = r#"{"protocol":1,"session_id":"s","worktree_id":1,"label":"l",
                      "cwd":"/tmp","pid":42,"exited":null}"#;
        let hello: Hello = serde_json::from_str(old).expect("an old greeting must still parse");
        assert_eq!(hello.protocol, PROTOCOL);
        assert!(
            !hello.supports_redraw,
            "an old holder must not be sent REDRAW: it would drop the connection \
             on a daemon-numbered frame it cannot ignore, costing a live terminal"
        );
        assert!(!hello.supports_busy, "same for QUERY_BUSY");
        assert_eq!(hello.detached_secs, None);
    }

    #[test]
    fn busy_payloads_round_trip_and_short_payloads_are_rejected() {
        assert_eq!(decode_busy(&encode_busy(true)), Some(true));
        assert_eq!(decode_busy(&encode_busy(false)), Some(false));
        // Reading a byte out of an empty payload would guess at the answer.
        assert_eq!(decode_busy(&[]), None);
        assert_eq!(decode_busy(&[0, 1]), None);
        assert_eq!(decode_busy(&[2]), None);
    }
}
