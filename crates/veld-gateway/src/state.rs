//! Shared application state handed to every request handler.

use std::sync::Arc;

use crate::config::GatewayConfig;
use crate::registry::Registry;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<GatewayConfig>,
    pub registry: Arc<Registry>,
    /// The resolved registration auth token. Resolved once at startup — never
    /// per request, so a `command` token source cannot be turned into a
    /// request-amplified process spawner. Rotation requires a restart
    /// (documented).
    pub auth_token: Arc<str>,
}
