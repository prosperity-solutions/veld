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
.veld-feedback-hover-outline {
  position: absolute;
  outline: 2px dashed var(--vfl-accent);
  outline-offset: 2px;
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
  pointer-events: none;
  z-index: 999998;
  border-radius: 3px;
  display: none;
}
.veld-feedback-draw-canvas {
  position: fixed; inset: 0;
  width: 100%; height: 100%;
  z-index: 999997;
  cursor: crosshair;
  touch-action: none;
  background: transparent;
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
