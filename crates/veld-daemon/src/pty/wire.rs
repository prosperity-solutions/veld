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

// Daemon → holder.
/// Keystrokes for the PTY, raw bytes.
pub const INPUT: u8 = 0x81;
/// Window size; payload is `cols` then `rows`, each `u16` big-endian.
pub const RESIZE: u8 = 0x82;
/// End the shell and exit. **Stable across every protocol version** — see the
/// module docs before touching this.
pub const HANGUP: u8 = 0x83;

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
    /// `Some(code)` if the shell has already exited.
    pub exited: Option<u32>,
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
        for kind in [INPUT, RESIZE, HANGUP, 0x99] {
            assert!(
                !Frame {
                    kind,
                    payload: vec![]
                }
                .is_ignorable(),
                "{kind:#x} must not be ignorable"
            );
        }
        for kind in [HELLO, SCROLLBACK, OUTPUT, EXIT, 0x7f] {
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
    fn hangup_is_pinned_to_its_byte() {
        // The one frame every version must understand. Changing this number
        // strands shells behind daemons that cannot ask them to stop.
        assert_eq!(HANGUP, 0x83);
        assert_eq!(PROTOCOL, 1);
    }
}
