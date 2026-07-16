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
            let via = strip_scheme(path.remote_addr().to_string());
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

/// Strip the scheme tag from a `TransportAddr` Display form: iroh renders
/// `relay:https://…`, `ip:1.2.3.4:5678`, `ip:[::1]:5678`, `custom:…` — the
/// tag duplicates what `transport` already says, so `via` carries only the
/// address. A value with no colon (not produced by iroh 1.x, kept as a
/// defensive fallback) passes through unchanged.
fn strip_scheme(addr: String) -> String {
    addr.split_once(':')
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or(addr)
}

#[cfg(test)]
mod tests {
    use super::strip_scheme;

    #[test]
    fn scheme_stripping_keeps_the_address_intact() {
        assert_eq!(
            strip_scheme("relay:https://euw1-1.relay.iroh.network./".into()),
            "https://euw1-1.relay.iroh.network./"
        );
        assert_eq!(
            strip_scheme("ip:203.0.113.7:4711".into()),
            "203.0.113.7:4711"
        );
        // IPv6: the bracketed address survives with its inner colons.
        assert_eq!(strip_scheme("ip:[::1]:4711".into()), "[::1]:4711");
        // No scheme tag → unchanged (defensive; iroh always tags).
        assert_eq!(strip_scheme("bare-value".into()), "bare-value");
    }
}
