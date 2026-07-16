//! Peer-to-peer environment sharing over iroh.
//!
//! The transport layer (wire protocol, endpoint/relay policy, host + join
//! halves) lives in the `veld-share` crate, shared with `veld-gateway` so the
//! protocol cannot drift between the daemon and the gateway. This module
//! re-exports it and adds what is daemon-specific: share/join lifecycle
//! management ([`manager`]), the HTTP control API ([`api`]), and the
//! interactive relay-token cache ([`token_store`]).
//!
//! The daemon hosts one long-lived iroh endpoint per relay policy. A *share*
//! exposes selected local services to token-bearing peers; a *join* dials a
//! share and materialises its URLs locally as Caddy routes over the tunnel.

pub use veld_share::{endpoint, host, join};

pub mod api;
pub mod manager;
pub mod token_store;
