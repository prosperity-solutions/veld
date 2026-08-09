use serde::{Deserialize, Serialize};

/// Incoming request from the CLI / daemon over the Unix socket.
#[derive(Debug, Deserialize)]
pub struct Request {
    pub command: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// Outgoing response sent back over the Unix socket.
#[derive(Debug, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A dispatched request's outcome: the reply to send, plus whether the helper
/// must exit once that reply is actually on the wire.
///
/// The flag exists to make an ordering structural instead of a timing bet.
/// Signalling the exit from inside a command handler races the socket write that
/// carries its reply — the process can be gone before the bytes leave — and the
/// caller then cannot tell "restarting" apart from "the helper died". Deciding
/// here and exiting in the connection loop, after the flush, removes the race.
pub struct Handled {
    pub response: Response,
    pub exit_after_reply: bool,
}

impl Handled {
    /// The ordinary case: reply and keep running.
    pub fn reply(response: Response) -> Self {
        Self {
            response,
            exit_after_reply: false,
        }
    }
}

impl Response {
    pub fn ok() -> Self {
        Self {
            ok: true,
            data: None,
            error: None,
        }
    }

    pub fn ok_with_data(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(message.into()),
        }
    }
}
