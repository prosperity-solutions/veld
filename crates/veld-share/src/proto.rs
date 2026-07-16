//! Wire protocol spoken over the iroh connection between a consumer and a host.
//!
//! One connection carries:
//! - a **control** bi-stream (opened first by the consumer): the consumer sends
//!   [`ControlRequest`], the host replies [`ControlResponse`];
//! - then one **data** bi-stream per proxied TCP connection: the consumer sends
//!   an [`OpenStream`] frame naming the target hostname, then raw bytes flow.
//!
//! Framing is a 4-byte big-endian length prefix followed by the payload.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use veld_core::share::{Capability, ShareManifest};

/// Maximum length of a single framed control message (payloads, not tunnel
/// data, which is unframed after the opening frame).
pub const MAX_FRAME: u32 = 64 * 1024;

/// Consumer → host, on the control stream.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ControlRequest {
    /// Bearer capability from the ticket (gate 1).
    pub capability: Capability,
    /// Self-asserted, untrusted human label shown to the host on approval.
    pub label: String,
}

/// Host → consumer, on the control stream. On approval it carries the manifest
/// (which URLs/ports to materialise) so the ticket doesn't have to.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ControlResponse {
    pub approved: bool,
    pub reason: Option<String>,
    pub manifest: Option<ShareManifest>,
}

impl ControlResponse {
    pub fn approved(manifest: ShareManifest) -> Self {
        Self {
            approved: true,
            reason: None,
            manifest: Some(manifest),
        }
    }
    pub fn denied(reason: impl Into<String>) -> Self {
        Self {
            approved: false,
            reason: Some(reason.into()),
            manifest: None,
        }
    }
}

/// Consumer → host, first frame on each data stream: which shared hostname this
/// TCP connection targets.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct OpenStream {
    pub hostname: String,
}

/// Write a length-prefixed frame.
pub async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.len() as u64 > MAX_FRAME as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "frame exceeds MAX_FRAME",
        ));
    }
    w.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    w.write_all(bytes).await?;
    w.flush().await?;
    Ok(())
}

/// Read a length-prefixed frame, rejecting oversized lengths.
pub async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len);
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame exceeds MAX_FRAME",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Write a JSON value as one frame.
pub async fn write_json<W, T>(w: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)?;
    write_frame(w, &bytes).await
}

/// Read one frame and parse it as JSON.
pub async fn read_json<R, T>(r: &mut R) -> std::io::Result<T>
where
    R: AsyncReadExt + Unpin,
    T: DeserializeOwned,
{
    let bytes = read_frame(r).await?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_round_trips_over_a_pipe() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let req = ControlRequest {
            capability: Capability::generate(),
            label: "Bob's MacBook".to_string(),
        };
        let sent = serde_json::to_vec(&req).unwrap();
        write_frame(&mut a, &sent).await.unwrap();
        let got = read_frame(&mut b).await.unwrap();
        assert_eq!(sent, got);

        let parsed: ControlRequest = serde_json::from_slice(&got).unwrap();
        assert_eq!(parsed.label, "Bob's MacBook");
    }

    #[tokio::test]
    async fn read_frame_rejects_oversized_length() {
        let (mut a, mut b) = tokio::io::duplex(64);
        // Hand-write a length prefix larger than MAX_FRAME.
        a.write_all(&(MAX_FRAME + 1).to_be_bytes()).await.unwrap();
        a.flush().await.unwrap();
        let err = read_frame(&mut b).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
