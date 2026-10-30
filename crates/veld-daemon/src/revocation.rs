//! Per-client revocation fences shared by tickets, streams, and PTY input.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::{OwnedRwLockReadGuard, RwLock, watch};

static CLIENTS: LazyLock<Mutex<HashMap<String, Arc<Client>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct Client {
    revoked: watch::Sender<bool>,
    pub(super) operations: Arc<RwLock<()>>,
}

impl Client {
    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.revoked.subscribe()
    }

    pub(crate) fn is_revoked(&self) -> bool {
        *self.revoked.borrow()
    }

    /// Fence registration against revoke. Drop releases the permit without an
    /// asynchronous deregistration step that could race the revocation snapshot.
    pub(crate) async fn authorize(self: &Arc<Self>) -> Option<OwnedRwLockReadGuard<()>> {
        let permit = self.operations.clone().read_owned().await;
        (!self.is_revoked()).then_some(permit)
    }

    pub(super) fn revoke(&self) {
        self.revoked.send_replace(true);
    }
}

pub(crate) fn client(client_id: &str) -> Arc<Client> {
    CLIENTS
        .lock()
        .expect("remote revocation registry poisoned")
        .entry(client_id.to_owned())
        .or_insert_with(|| {
            Arc::new(Client {
                revoked: watch::channel(false).0,
                operations: Arc::new(RwLock::new(())),
            })
        })
        .clone()
}

/// Subscribe before opening a remote stream. Revocation is monotone for the
/// process lifetime; a subscription made after revoke starts already reads true.
pub(crate) fn subscribe(client_id: &str) -> watch::Receiver<bool> {
    client(client_id).subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revoke_fences_registration_without_async_deregistration() {
        let id = uuid::Uuid::new_v4().to_string();
        let client = client(&id);
        let permit = client.authorize().await.unwrap();
        client.revoke();
        assert!(*subscribe(&id).borrow());
        let barrier = client.operations.clone().try_write_owned();
        assert!(barrier.is_err(), "registration escaped the revoke fence");
        drop(permit);
        assert!(client.operations.clone().try_write_owned().is_ok());
        assert!(client.authorize().await.is_none());
    }
}
