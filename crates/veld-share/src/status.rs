//! Transport introspection for a live share connection.
//!
//! Answers the operator question "is this tunnel direct or riding a relay?" —
//! the difference between LAN-speed shares and ones capped by a (possibly
//! throttled) relay server. Shared between the daemon (share status API) and
//! the gateway (registration logging) so both report the same picture.

use iroh::endpoint::Connection;
use veld_core::share::{ShareConnectionInfo, ShareTransport};

/// Snapshot one connection's selected path into a [`ShareConnectionInfo`].
///
/// iroh keeps a relay path open and hole-punches a direct path alongside it;
/// the **selected** path is the one application data actually rides, so that
/// is the one reported. `label` is the peer's untrusted self-label (join
/// requests) or a caller-chosen role ("host" on the join side).
pub fn connection_info(conn: &Connection, label: &str) -> ShareConnectionInfo {
    let node_id = conn.remote_id().to_string();
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected());
    let (transport, via, rtt_ms) = match &selected {
        Some(path) => {
            let transport = if path.is_relay() {
                ShareTransport::Relayed
            } else {
                ShareTransport::Direct
            };
            // Display forms are `relay:https://…` / `ip:1.2.3.4:5` — strip the
            // scheme tag; the transport field already carries it.
            let addr = path.remote_addr().to_string();
            let via = addr
                .split_once(':')
                .map(|(_, rest)| rest.to_owned())
                .unwrap_or(addr);
            let rtt = u64::try_from(path.rtt().as_millis()).unwrap_or(u64::MAX);
            (transport, Some(via), Some(rtt))
        }
        // A snapshot can catch a connection with no selected path (path
        // migration, or the peer just dropped) — report it honestly instead
        // of guessing.
        None => (ShareTransport::None, None, None),
    };
    ShareConnectionInfo {
        node_id,
        label: label.to_owned(),
        transport,
        via,
        rtt_ms,
    }
}
