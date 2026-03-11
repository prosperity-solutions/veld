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
