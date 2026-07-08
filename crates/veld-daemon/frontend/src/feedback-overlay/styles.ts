// CSS is imported as text by esbuild (loader: { ".css": "text" })
import SHADOW_CSS from "../feedback-overlay.css";
export { SHADOW_CSS };

// Light DOM CSS — for elements that must live in document.body.
// Uses CSS custom properties on the veld-feedback host element so
// light DOM children can inherit theme colors.
export const LIGHT_CSS = `
/* Theme variables for light DOM — inherited from veld-feedback host */
:root {
  --vfl-bg: #0a0a0a;
  --vfl-bg-card: #1e1e24;
  --vfl-text: #f1f5f9;
  --vfl-text-muted: #94a3b8;
  --vfl-accent: #C4F56A;
  --vfl-danger: #ef4444;
  --vfl-border: #2a2a30;
  --vfl-font: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
}
@media (prefers-color-scheme: dark) {
  :root {
    --vfl-bg: #f8f8fa;
    --vfl-bg-card: #eeeef2;
    --vfl-text: #1a1a2e;
    --vfl-text-muted: #64748b;
    --vfl-accent: #16a34a;
    --vfl-danger: #dc2626;
    --vfl-border: #d4d4d8;
  }
}
:root[data-veld-theme="dark"] {
  --vfl-bg: #0a0a0a;
  --vfl-bg-card: #1e1e24;
  --vfl-text: #f1f5f9;
  --vfl-text-muted: #94a3b8;
  --vfl-accent: #C4F56A;
  --vfl-danger: #ef4444;
  --vfl-border: #2a2a30;
}
:root[data-veld-theme="light"] {
  --vfl-bg: #f8f8fa;
  --vfl-bg-card: #eeeef2;
  --vfl-text: #1a1a2e;
  --vfl-text-muted: #64748b;
  --vfl-accent: #16a34a;
  --vfl-danger: #dc2626;
  --vfl-border: #d4d4d8;
}

[class^="veld-feedback-"],
[class*=" veld-feedback-"] {
  box-sizing: border-box;
}
.veld-feedback-overlay {
  position: fixed; inset: 0;
  z-index: 999997;
  background: rgba(10,10,10,.08);
  display: none; cursor: pointer;
}
.veld-feedback-overlay-active { display: block; }
.veld-feedback-overlay-crosshair { cursor: crosshair; }
/* Screenshot mode: the frame is painted as a background-image with
   background-size:contain (see screenshot.ts), so it can letterbox instead
   of stretching when its aspect ratio doesn't match the viewport. This near-
   opaque backing makes the letterbox bars read as an intentional frame edge
   rather than the live page bleeding through. */
.veld-feedback-overlay-frame { background-color: rgba(8,8,10,.92); }
.veld-feedback-hover-outline {
  position: absolute;
  outline: 2px solid var(--vfl-accent);
  outline-offset: 2px;
  /* Dark backdrop with the hovered element cut out: the huge box-shadow dims
     the whole page while the element's own box stays clear (a spotlight). */
  box-shadow: 0 0 0 9999px rgba(0, 0, 0, 0.45);
  pointer-events: none;
  z-index: 999998;
  border-radius: 3px;
  transition: top .1s, left .1s, width .1s, height .1s;
  display: none;
}
.veld-feedback-component-trace {
  position: absolute; z-index: 999999;
  background: var(--vfl-bg); color: var(--vfl-accent);
  padding: 4px 10px; border-radius: 6px;
  font: 500 11px/1.4 var(--vfl-font);
  pointer-events: none; white-space: nowrap;
  box-shadow: 0 2px 10px rgba(0,0,0,.15);
  border: 1px solid var(--vfl-border);
  display: none;
}
.veld-feedback-screenshot-rect {
  position: absolute;
  outline: 2px dashed var(--vfl-accent);
  outline-offset: 2px;
  background: rgba(100,100,100,.06);
  /* Same spotlight trick as the hover-outline: dims everything outside the
     drawn selection so it's unambiguous what will be captured. */
  box-shadow: 0 0 0 9999px rgba(0, 0, 0, 0.55);
  pointer-events: none;
  z-index: 999998;
  border-radius: 3px;
  display: none;
}
.veld-feedback-screenshot-corner {
  position: absolute;
  width: 16px; height: 16px;
  border: 2px solid var(--vfl-accent);
  pointer-events: none;
}
.veld-feedback-screenshot-corner-tl { top: -2px; left: -2px; border-right: none; border-bottom: none; }
.veld-feedback-screenshot-corner-tr { top: -2px; right: -2px; border-left: none; border-bottom: none; }
.veld-feedback-screenshot-corner-bl { bottom: -2px; left: -2px; border-right: none; border-top: none; }
.veld-feedback-screenshot-corner-br { bottom: -2px; right: -2px; border-left: none; border-top: none; }
.veld-feedback-screenshot-banner {
  position: fixed; top: 20px; left: 50%; transform: translateX(-50%);
  z-index: 1000000;
  /* The banner sits above the drag surface, so a selection that starts under
     it (e.g. capturing a page header near the top of the viewport) would
     otherwise never reach the overlay's mousedown handler. Disable hit-
     testing on the banner itself and re-enable it only on the button. */
  pointer-events: none;
  display: none; align-items: center; gap: 10px;
  background: var(--vfl-bg); color: var(--vfl-text);
  border: 1px solid var(--vfl-border); border-radius: 10px;
  padding: 10px 14px;
  font: 500 12px/1.4 var(--vfl-font);
  box-shadow: 0 8px 30px rgba(0,0,0,.4);
}
.veld-feedback-screenshot-banner-show { display: flex; }
.veld-feedback-screenshot-banner-text { white-space: nowrap; }
.veld-feedback-screenshot-banner-hint { color: var(--vfl-text-muted); font-size: 11px; white-space: nowrap; }
.veld-feedback-screenshot-banner-btn {
  pointer-events: auto;
  padding: 5px 12px; border-radius: 6px; border: none; cursor: pointer;
  background: var(--vfl-accent); color: var(--vfl-bg);
  font: 600 11px/1.4 var(--vfl-font); white-space: nowrap;
}
.veld-feedback-pin {
  position: absolute; z-index: 999998;
  display: flex; align-items: center; gap: 3px;
  padding: 3px 8px;
  background: var(--vfl-bg); color: var(--vfl-text);
  border: 1px solid var(--vfl-border);
  border-radius: 16px;
  cursor: pointer;
  box-shadow: 0 2px 8px rgba(0,0,0,.12);
  transition: transform .15s, border-color .15s;
  font: 500 10px/1 var(--vfl-font);
}
.veld-feedback-pin:hover { transform: scale(1.1); border-color: var(--vfl-accent); }
.veld-feedback-pin-icon svg { width: 14px; height: 14px; color: var(--vfl-accent); }
.veld-feedback-pin-count { font: 700 10px/1 var(--vfl-font); color: var(--vfl-text-muted); }
.veld-feedback-pin-unread-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--vfl-danger); flex-shrink: 0; }
.veld-feedback-pin-highlight { animation: veld-feedback-pin-pulse 1.5s ease; }
@keyframes veld-feedback-pin-pulse {
  0% { box-shadow: 0 0 0 0 rgba(100,200,100,.5); transform: scale(1); }
  50% { box-shadow: 0 0 0 10px rgba(100,200,100,0); transform: scale(1.1); }
  100% { box-shadow: 0 0 0 0 rgba(100,200,100,0); transform: scale(1); }
}
.veld-feedback-hidden {
  opacity: 0 !important;
  transform: scale(0.85) !important;
  pointer-events: none !important;
}
`;
