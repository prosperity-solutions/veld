//! Feedback overlay assets served by the feedback HTTP server.
//!
//! The overlay `<script>` and `<link>` tags are injected into HTML responses by
//! Caddy's `replace-response` plugin — no Service Worker or manual activation
//! needed.

/// Self-contained feedback overlay UI script.
pub const OVERLAY_JS: &str = include_str!("../assets/feedback-overlay.js");

/// Feedback overlay CSS stylesheet.
pub const OVERLAY_CSS: &str = include_str!("../assets/feedback-overlay.css");

/// Veld logo SVG mark.
pub const LOGO_SVG: &str = include_str!("../assets/logo.svg");
