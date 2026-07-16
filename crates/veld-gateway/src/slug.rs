//! Deterministic, unguessable public-URL slugs (SHARING_V2.md §5.3).
//!
//! ```text
//! slug = base32( SHA-256("veld-gateway-slug/1" ‖ host_node_id ‖ len(hostname) ‖ hostname ‖ capability)[..16] )
//! ```
//!
//! Properties, in order of intent:
//!
//! - **Stateless**: recomputable from the registration alone, so a gateway
//!   restart followed by the origin's heartbeat re-register yields the same
//!   public URL — no database.
//! - **Machine-bound**: the host's iroh node id is an input, so the same
//!   service shared from a different machine gets a different URL.
//! - **Unguessable**: the 32-byte share capability is an input; the slug
//!   inherits its entropy through a one-way hash, so the URL itself is the
//!   baseline bearer secret and leaks nothing about the capability.
//! - **DNS-safe**: 16 bytes → 26 lowercase base32 chars, inside the 63-char
//!   label limit and free of ambiguous characters.

use data_encoding::BASE32_NOPAD;
use iroh::EndpointId;
use sha2::{Digest, Sha256};
use veld_core::share::Capability;

/// Domain-separation tag; bump the suffix if the derivation ever changes so
/// old and new gateways can never mint colliding-but-different URL schemes.
const TAG: &[u8] = b"veld-gateway-slug/1";

/// Derive the slug for one shared hostname of one share.
pub fn derive(host_node_id: &EndpointId, hostname: &str, capability: &Capability) -> String {
    let mut h = Sha256::new();
    h.update(TAG);
    h.update(host_node_id.as_bytes());
    // Length-prefix the only variable-length field so field boundaries are
    // unambiguous (no way to shift bytes between hostname and capability).
    h.update((hostname.len() as u16).to_be_bytes());
    h.update(hostname.as_bytes());
    h.update(capability.as_bytes());
    let digest = h.finalize();
    BASE32_NOPAD.encode(&digest[..16]).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn node_id() -> EndpointId {
        SecretKey::generate().public()
    }

    #[test]
    fn slug_is_deterministic_and_dns_safe() {
        let id = node_id();
        let cap = Capability::generate();
        let a = derive(&id, "app.demo.p.localhost", &cap);
        let b = derive(&id, "app.demo.p.localhost", &cap);
        assert_eq!(a, b, "same inputs must yield the same slug (statelessness)");
        assert_eq!(a.len(), 26);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "slug must be a valid lowercase DNS label: {a}"
        );
    }

    #[test]
    fn slug_varies_with_every_input() {
        let id = node_id();
        let cap = Capability::generate();
        let base = derive(&id, "app.demo.p.localhost", &cap);

        // Different hostname → different slug.
        assert_ne!(base, derive(&id, "api.demo.p.localhost", &cap));
        // Different capability (a new share) → different slug.
        assert_ne!(
            base,
            derive(&id, "app.demo.p.localhost", &Capability::generate())
        );
        // Different host machine → different slug (machine-bound).
        assert_ne!(base, derive(&node_id(), "app.demo.p.localhost", &cap));
    }

    #[test]
    fn field_boundaries_are_unambiguous() {
        // Without the length prefix, ("ab", cap) and ("a", b‖cap-shifted) could
        // collide. The prefix makes hostname/capability boundaries explicit —
        // spot-check that a boundary shift changes the slug.
        let id = node_id();
        let cap = Capability::generate();
        assert_ne!(
            derive(&id, "app.x", &cap),
            derive(&id, "app.xx", &cap),
            "hostname prefix collision"
        );
    }
}
