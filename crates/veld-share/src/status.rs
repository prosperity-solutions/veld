//! Transport introspection for a live share connection.
//!
//! Answers the operator question "is this tunnel direct or riding a relay?" —
//! the difference between LAN-speed shares and ones capped by a (possibly
//! throttled) relay server. Shared between the daemon (share status API) and
//! the gateway (registration logging) so both report the same picture.

use iroh::TransportAddr;
use iroh::endpoint::Connection;
use veld_core::share::{ShareConnectionInfo, ShareTransport};

/// Longest peer label the snapshot carries. Labels are peer-chosen; the cap
/// keeps a hostile joiner from bloating every status response and log line.
const MAX_LABEL_CHARS: usize = 120;

/// Snapshot one connection's selected path into a [`ShareConnectionInfo`].
///
/// iroh keeps a relay path open and hole-punches a direct path alongside it;
/// the **selected** path is the one application data actually rides, so that
/// is the one reported. `label` is the peer's untrusted self-label (join
/// requests) or a caller-chosen role ("host" on the join side) — it is
/// sanitized here, at the source, so every sink (terminal, logs, JSON for the
/// UIs) gets a string that cannot smuggle ANSI escapes or newlines.
pub fn connection_info(conn: &Connection, label: &str) -> ShareConnectionInfo {
    let node_id = conn.remote_id().to_string();
    let paths = conn.paths();
    let selected = paths.iter().find(|p| p.is_selected());
    let (transport, via, rtt_ms) = match &selected {
        Some(path) => {
            // Match the address variants rather than parsing the Display
            // form — a future iroh Display tweak must not corrupt `via`.
            let (transport, via) = match path.remote_addr() {
                TransportAddr::Relay(url) => (ShareTransport::Relayed, url.to_string()),
                TransportAddr::Ip(addr) => (ShareTransport::Direct, addr.to_string()),
                // Custom transports (unused by veld) are direct-class: not
                // relay-mediated, so not subject to relay throttling.
                other => (ShareTransport::Direct, strip_scheme(other.to_string())),
            };
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
        label: sanitize_label(label),
        transport,
        via,
        rtt_ms,
    }
}

/// Neutralize a peer-chosen label for display: control characters (ANSI
/// escapes, newlines, tabs) and Unicode bidi/isolate overrides (U+202E and
/// friends — the secondary terminal/reader-spoofing vector) are dropped, and
/// the length is capped. `veld shares` prints labels to the host's terminal,
/// where `\x1b[…` would otherwise let an approved joiner rewrite the screen;
/// daemons also sanitize at ingestion (accept loop) so log lines and the
/// pending-approval list carry clean labels too.
pub fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_override(*c))
        .take(MAX_LABEL_CHARS)
        .collect()
}

/// Unicode directional-override and isolate controls (format chars, so not
/// caught by `is_control`): LRM/RLM/ALM, LRE..RLO, LRI..PDI.
fn is_bidi_override(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Strip the scheme tag from a `TransportAddr` Display form (`custom:…` for
/// the non-exhaustive fallback arm above — the tag duplicates what
/// `transport` already says). A value with no colon passes through unchanged.
fn strip_scheme(addr: String) -> String {
    addr.split_once(':')
        .map(|(_, rest)| rest.to_owned())
        .unwrap_or(addr)
}

#[cfg(test)]
mod tests {
    use super::{sanitize_label, strip_scheme};

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
        // No scheme tag → unchanged (defensive).
        assert_eq!(strip_scheme("bare-value".into()), "bare-value");
    }

    #[test]
    fn labels_cannot_smuggle_ansi_or_newlines() {
        // An ANSI clear-screen + fake status line, as a hostile joiner would
        // send it: every control char is dropped, text survives.
        assert_eq!(
            sanitize_label("evil\x1b[2J\x1b[1;31mFAKE\nline\ttab"),
            "evil[2J[1;31mFAKElinetab"
        );
        assert_eq!(sanitize_label("alice's laptop"), "alice's laptop");
        // Bidi overrides (format chars, not control chars) are dropped too.
        assert_eq!(sanitize_label("abc\u{202E}gpj.exe"), "abcgpj.exe");
        // Length is capped.
        assert_eq!(sanitize_label(&"x".repeat(500)).chars().count(), 120);
    }
}
