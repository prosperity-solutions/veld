//! Peer-to-peer environment sharing over iroh.
//!
//! The daemon hosts a single long-lived iroh [`Endpoint`](iroh::Endpoint).
//! A *share* exposes selected local services to token-bearing peers; a *join*
//! dials a share and materialises its URLs locally as Caddy routes over the
//! tunnel. See `RFC-p2p-sharing.md` and `PLAN-p2p-implementation.md`.
//!
//! Phase 0 lays the transport foundation (endpoint + stream splice). Later
//! phases add the control protocol, manifest, approval flow, and CLI/dashboard
//! surfaces; the transport primitives here are consumed then.

// Phase 0 scaffolding: these primitives are wired into the daemon's control
// plane in Phase 2. Allow until then so `clippy -D warnings` stays green.
#![allow(dead_code)]

pub mod endpoint;
pub mod forward;

#[cfg(test)]
mod tests {
    use super::endpoint::{load_or_create_secret_key, ALPN};

    #[test]
    fn alpn_is_versioned() {
        assert_eq!(ALPN, b"veld/share/1");
    }

    #[test]
    fn secret_key_persists_and_reloads() {
        let path = std::env::temp_dir().join(format!(
            "veld-node-key-test-{}-{}",
            std::process::id(),
            // vary per test invocation without needing rand
            line!()
        ));
        let _ = std::fs::remove_file(&path);

        let first = load_or_create_secret_key(&path).expect("create key");
        let second = load_or_create_secret_key(&path).expect("reload key");

        assert_eq!(
            first.public(),
            second.public(),
            "reloaded key must yield the same public identity"
        );

        let _ = std::fs::remove_file(&path);
    }
}
