//! Feedback overlay assets served by the feedback HTTP server.
//!
//! Built assets (JS/CSS) are produced by the frontend build pipeline
//! (TypeScript → esbuild) and placed in OUT_DIR by build.rs.
//! Static assets (SVGs, HTML) are included directly from the assets/ directory.

/// Self-contained feedback overlay UI script (CSS is bundled in via Shadow DOM).
pub const OVERLAY_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/feedback-overlay.js"));

/// Canvas drawing / annotation engine (lazy-loaded by feedback overlay).
pub const DRAW_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/draw-overlay.js"));

/// Client-side log collector script (injected into HTML `<head>` by Caddy).
pub const CLIENT_LOG_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/client-log.js"));

/// Veld logo SVG mark.
pub const LOGO_SVG: &str = include_str!("../assets/logo.svg");
