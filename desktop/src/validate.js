// Validation for everything the page hands the shell.
//
// Its own module for one reason: this is the **trust boundary**, and it was
// previously inline in `browserViews.js` where nothing was exported and the
// desktop package had no test runner — so the copy that actually guards the main
// process was the one with no tests, while its renderer-side counterpart
// (`normalizeBrowserUrl` in `crates/veld-daemon/ui/src/panes/model.ts`) was
// covered. The renderer's checks are defence-in-depth; these are the real gate,
// because a renderer is not a trust boundary.
//
// Pure and dependency-free, so `node --test src/validate.test.js` runs it without
// an Electron binary.

/**
 * View ids come from the page. Kept to the same charset the daemon accepts for a
 * PTY session id, since a browser tab id and a terminal tab id are minted by the
 * same generator (`newTabId`).
 */
const ID_RE = /^[A-Za-z0-9_-]{1,64}$/;

/**
 * Session slot names, which become the tail of an Electron partition string.
 * Lowercase, no dots, no slashes, bounded — so a name can never traverse or
 * collide with another namespace's partition.
 */
const PROFILE_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;

/**
 * Only `http(s)`. A browser pane is a preview of a dev server, and every other
 * scheme a URL parser accepts turns one into something else — `file:` into a
 * local-file reader, `javascript:` into script in the pane's own origin, `blob:`
 * and `data:` into content with no origin to attribute.
 *
 * Returns the normalised URL, or `null` to reject. Never throws.
 */
function safeUrl(raw) {
  if (typeof raw !== "string" || raw === "") return null;
  let u;
  try {
    u = new URL(raw);
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  return u.toString();
}

/** Whether the page may address a view by this id. */
function isViewId(id) {
  return typeof id === "string" && ID_RE.test(id);
}

/** Whether the page may name this session slot. */
function isProfileName(name) {
  return typeof name === "string" && PROFILE_RE.test(name);
}

/** `persist:` so a slot's cookies survive a restart — the point of naming one.
 *  Namespaced so it can never collide with the app's own session. */
function partitionFor(profile) {
  return `persist:veld-browser-${profile}`;
}

/**
 * Emulated viewport bounds, mirroring `MIN_DEVICE_PX`/`MAX_DEVICE_PX` in
 * `crates/veld-daemon/ui/src/panes/devices.ts`.
 *
 * Restated rather than shared for the same reason `safeUrl` restates
 * `normalizeBrowserUrl`: this is the copy that guards the main process, and the
 * renderer's is defence-in-depth. The renderer's numbers are what the pane's
 * controls offer; these are what the shell will actually apply.
 */
const MIN_DEVICE_PX = 120;
const MAX_DEVICE_PX = 4096;
const MAX_UA_LEN = 512;
const MIN_ZOOM = 0.25;
const MAX_ZOOM = 3;
/** A screen scaled below this is a few pixels of page; treat it as the floor
 *  rather than rendering into nothing. */
const MIN_SCALE = 0.02;
const MAX_DEVICE_RADIUS = 64;

/**
 * A user-agent string the shell is willing to put in a request header, or `null`.
 *
 * This is the one field of an emulation that leaves the process as protocol
 * rather than as geometry, so it is the one with a real threat behind it:
 * `setUserAgent` takes a header value, and a CR or LF in it is header injection
 * against every origin the pane visits. Printable ASCII only, bounded, and
 * **rejected rather than repaired** — a UA with a newline in it is not a UA with
 * a typo.
 */
function safeUserAgent(raw) {
  if (typeof raw !== "string") return null;
  const text = raw.trim();
  if (text === "" || text.length > MAX_UA_LEN) return null;
  if (!/^[\x20-\x7e]+$/.test(text)) return null;
  return text;
}

function clampDevicePx(n) {
  const v = Number(n);
  if (!Number.isFinite(v)) return null;
  return Math.min(MAX_DEVICE_PX, Math.max(MIN_DEVICE_PX, Math.round(v)));
}

/**
 * The device-emulation parameters the page asked for, normalised, or `null`.
 *
 * `null` means "no emulation", which is also what an unusable payload degrades
 * to: a view showing itself at pane size is a correct state, while a view at a
 * size derived from `NaN` is not. Only the fields Electron's
 * `enableDeviceEmulation` consumes come out of here — the preset id the pane
 * labels its menu with stays in the renderer, because the shell has no use for
 * it and no way to check it.
 */
function safeEmulation(raw) {
  if (typeof raw !== "object" || raw === null) return null;
  const width = clampDevicePx(raw.width);
  const height = clampDevicePx(raw.height);
  if (width === null || height === null) return null;
  const dsf = Number(raw.deviceScaleFactor);
  return {
    width,
    height,
    // Electron types this `Integer`, and 0 means "the host display's own".
    deviceScaleFactor: Number.isFinite(dsf) ? Math.min(4, Math.max(0, Math.round(dsf))) : 0,
    mobile: raw.mobile === true,
    touch: raw.touch === true,
    userAgent: safeUserAgent(raw.ua),
    // No `fit`. It reaches this process on the wire and is deliberately dropped
    // here: fitting is a question about the *pane*, answered by `deviceLayout` in
    // the renderer, which then sends the resulting factor with the bounds. A
    // validated field with no reader is an invitation to make the shell re-derive
    // the scale, which is the two-owners drift that split ended.
  };
}

/** A page zoom factor within Chromium's own range, or `null`. `setZoomFactor`
 *  throws on a non-positive one, and the pane is not a trusted caller. */
function safeZoom(raw) {
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) return null;
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, n));
}

/**
 * The factor an emulated viewport is rendered at inside the view's box, clamped.
 *
 * Computed by the renderer, which owns the pane's geometry (`deviceLayout` in
 * `panes/devices.ts`) — the shell only has to make sure the number it applies is
 * usable. Never above 1: emulation shrinks a screen to fit a pane, and magnifying
 * one is what page zoom is for. Never at zero either, which would render the page
 * into nothing and look identical to a broken view.
 */
function safeScale(raw) {
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) return 1;
  return Math.min(1, Math.max(MIN_SCALE, n));
}

/** The screen's corner radius, in the pixels the view is actually drawn at.
 *  Already scaled by the renderer, so this only bounds it. */
function safeRadius(raw) {
  const n = Number(raw);
  if (!Number.isFinite(n) || n <= 0) return 0;
  return Math.min(MAX_DEVICE_RADIUS, Math.round(n));
}

module.exports = {
  ID_RE,
  PROFILE_RE,
  safeUrl,
  isViewId,
  isProfileName,
  partitionFor,
  safeUserAgent,
  safeEmulation,
  safeZoom,
  safeScale,
  safeRadius,
};
