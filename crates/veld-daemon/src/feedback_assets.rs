//! Feedback overlay assets served by the feedback HTTP server.
//!
//! The overlay scripts and stylesheets are loaded dynamically by a bootstrap
//! `<script>` tag that Caddy's `veld_inject` handler prepends to HTML
//! responses — no Service Worker or manual activation needed.

/// Self-contained feedback overlay UI script.
pub const OVERLAY_JS: &str = include_str!("../assets/feedback-overlay.js");

/// Feedback overlay CSS stylesheet.
pub const OVERLAY_CSS: &str = include_str!("../assets/feedback-overlay.css");

/// Veld logo SVG mark.
pub const LOGO_SVG: &str = include_str!("../assets/logo.svg");

/// Client-side log collector script (injected into HTML `<head>` by Caddy).
pub const CLIENT_LOG_JS: &str = include_str!("../assets/client-log.js");
