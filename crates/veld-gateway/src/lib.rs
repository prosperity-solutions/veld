//! veld-gateway — the public web gateway for Veld sharing (SHARING_V2.md §5).
//!
//! A headless Veld peer that joins shares over iroh and reverse-proxies the
//! tunneled services onto real public URLs (`https://<slug>.<domain>`).
//! Library layout (the binary in `main.rs` is a thin wrapper):
//!
//! - [`auth`] — viewer access control (passwords, stateless sessions)
//! - [`config`] — env-var-first configuration
//! - [`registry`] — join engine, slug routing table, lease bookkeeping
//! - [`api`] — Bearer-gated registration API (apex domain)
//! - [`proxy`] — the HTTP front for slug hosts (incl. WebSocket upgrades)
//! - [`tunnel`] — HTTP/1.1 client connections over iroh tunnel streams
//! - [`slug`] — deterministic, unguessable public-URL slugs
//! - [`server`] — listener, Host-based dispatch, graceful shutdown
//! - [`state`] — shared request state (config, registry, resolved auth token)

pub mod api;
pub mod auth;
pub mod config;
pub mod proxy;
pub mod registry;
pub mod server;
pub mod slug;
pub mod state;
pub mod tunnel;
