use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::{Mutex, broadcast};
use tracing::{debug, warn};

/// Capacity for the broadcast channel.
const CHANNEL_CAPACITY: usize = 256;

/// The broadcaster manages connected CLI clients and sends them JSON-encoded
/// state-change events over Unix sockets.
#[derive(Clone)]
pub struct Broadcaster {
    tx: broadcast::Sender<String>,
    /// Track the number of active clients (informational).
    client_count: Arc<Mutex<usize>>,
}

impl Broadcaster {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            tx,
            client_count: Arc::new(Mutex::new(0)),
        }
    }

    /// Broadcast a JSON event to all connected clients.
    pub async fn broadcast(&self, event: &serde_json::Value) {
        let msg = match serde_json::to_string(event) {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to serialise event: {e}");
                return;
            }
        };

        // Append a newline so clients can read line-by-line.
        let line = format!("{msg}\n");

        // broadcast::Sender::send returns Err only if there are no receivers,
        // which is fine -- just means no clients are connected.
        let _ = self.tx.send(line);
    }

    /// Handle a single connected client. Subscribes to the broadcast channel
    /// and forwards events until the client disconnects or the channel closes.
    pub async fn handle_client(&self, mut stream: UnixStream) {
        let mut rx = self.tx.subscribe();

        {
            let mut count = self.client_count.lock().await;
            *count += 1;
            debug!("client connected ({} total)", *count);
        }

        // Send a welcome message so the client knows the connection is live.
        let welcome = serde_json::json!({
            "event": "connected",
            "daemon_version": env!("CARGO_PKG_VERSION"),
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });

        if let Ok(msg) = serde_json::to_string(&welcome) {
            let line = format!("{msg}\n");
            if stream.write_all(line.as_bytes()).await.is_err() {
                self.client_disconnected().await;
                return;
            }
        }

        // Forward broadcast events to this client.
        loop {
            match rx.recv().await {
                Ok(line) => {
                    if stream.write_all(line.as_bytes()).await.is_err() {
                        debug!("client write failed, disconnecting");
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("client lagged by {n} messages");
                    // Continue -- the client will get the next message.
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!("broadcast channel closed");
                    break;
                }
            }
        }

        self.client_disconnected().await;
    }

    async fn client_disconnected(&self) {
        let mut count = self.client_count.lock().await;
        *count = count.saturating_sub(1);
        debug!("client disconnected ({} remaining)", *count);
    }
}
