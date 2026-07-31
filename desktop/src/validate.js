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
// Field parity with `PaneEmulation` (`crates/veld-daemon/ui/src/panes/devices.ts`) is
// maintained by hand: this is plain JS, so nothing type-checks the two shapes against
// each other. A field added there and forgotten here is silently *dropped at this
// boundary* — working in a browser tab, absent in the desktop app, which is this
// codebase's worst failure shape. The renderer holds the drift gate (a test asserting
// the exact key set); this is the pointer from the side that does the dropping.
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

/**
 * A CSS hex colour the shell is willing to hand to `setBackgroundColor`, or `null`.
 *
 * The page sends its own theme's surface colour so a view does not flash white in a dark
 * app before the guest paints. Hex only, and matched whole: Chromium accepts a broad
 * colour syntax, and there is no reason for this to be a place where a page-supplied
 * string is parsed liberally.
 */
function safeColor(raw) {
  if (typeof raw !== "string") return null;
  const text = raw.trim();
  return /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/.test(text) ? text : null;
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

// ---------------------------------------------------------------------------
// Window transfers (detach / hand-back)
// ---------------------------------------------------------------------------
//
// A tab record travels renderer → main → *another* renderer: out of the window
// it is being pulled from, held by the main process, and into the layout of the
// window that receives it — over `veld:window:seed` on a detach, or the
// `pendingAdopt` queue on a hand-back.
//
// The **semantic** gate for one of these is `parseTab` in
// `crates/veld-daemon/ui/src/panes/model.ts`, which every restored layout
// already goes through — it re-validates the URL, the emulation and the zoom on
// the way in, and it is the copy with the tests. Restating it here would be a
// second owner of a shape that has already drifted once, so this side checks
// only what the *shell* is exposed to and the renderer cannot check for itself —
// that the payload is structurally a tab, and that it is small enough for the
// main process to hold and hand on.

/** Pane kinds, mirroring `PANE_KINDS` in `panes/model.ts`. A kind missing here
 *  is a tab that cannot be detached; a stale extra one is inert, because the
 *  receiving renderer validates the kind again. */
const PANE_KINDS = ["terminal", "browser", "logs", "nodes", "new"];

/**
 * Ceiling on a serialized seed, in **UTF-8 bytes**.
 *
 * Bytes rather than JavaScript string length, because the two differ by up to 4×
 * and the earlier version measured the wrong one. A tab's `title` comes from a
 * page the user is previewing — arbitrary web content — so a document title of
 * 50 000 CJK characters produced a 50 KB string that passed a 64 KB check and a
 * 200 KB payload that did not. It mattered because the seed used to ride the new
 * process's command line, and an over-long argument fails the *launch*: the
 * window never appeared, after the origin had already released its tabs.
 *
 * The seed no longer goes near argv (see `veld:window:seed` in `windows.js`), so
 * this is now a bound on what the main process will hold and hand over. Kept,
 * and kept honest, because "we moved it, so the size stopped mattering" is how
 * the next transport inherits the same bug.
 */
const MAX_SEED_BYTES = 65536;

/**
 * Ceiling on one tab, in UTF-8 bytes.
 *
 * `safeTransferTab` deliberately carries unknown fields through, so the tab is
 * the place a page-controlled string can grow without limit — and a *snapshot*
 * is retained in the main process and re-copied on every layout change, with no
 * seed-sized total to stop it. `title` is truncated rather than rejected,
 * because a long page title is ordinary; anything still oversized after that is
 * not a tab this shell needs to carry.
 */
const MAX_TAB_BYTES = 8192;

function utf8Length(text) {
  return Buffer.byteLength(text, "utf8");
}

/** How many tabs one transfer may carry. Two docks of a window that has hit its
 *  view budget, with room to spare. */
const MAX_TRANSFER_TABS = 64;

/** Titles are shown in a native title bar and nowhere else; bound them and strip
 *  the control characters a title bar would render as boxes. */
const MAX_TITLE_LEN = 200;

function safeTitle(raw) {
  if (typeof raw !== "string") return null;
  // biome-ignore lint/suspicious/noControlCharactersInRegex: stripping them is the point.
  const text = raw.replace(/[\x00-\x1f\x7f]/g, " ").trim();
  return text === "" ? null : text.slice(0, MAX_TITLE_LEN);
}

/** A worktree id, which is a SQLite rowid on the daemon side. */
function safeWorktreeId(raw) {
  const n = Number(raw);
  return Number.isSafeInteger(n) && n > 0 ? n : null;
}

/**
 * A repository root, which the shell only ever puts in the `?repo=` parameter of
 * a URL it builds itself.
 *
 * Not checked against the filesystem: the shell has no opinion about which
 * checkouts exist, the daemon does, and a `?repo=` that resolves to nothing
 * already falls back to the first repo in the UI. Bounded and control-character
 * free so it cannot bloat or split the URL it lands in.
 */
function safeRepoRoot(raw) {
  if (typeof raw !== "string") return null;
  if (raw === "" || raw.length > 4096) return null;
  // biome-ignore lint/suspicious/noControlCharactersInRegex: rejecting them is the point.
  if (/[\x00-\x1f\x7f]/.test(raw)) return null;
  return raw;
}

/**
 * One transferred tab, structurally, or `null`.
 *
 * Unknown fields are carried through rather than dropped — the receiving
 * renderer's `parseTab` is what decides which of them mean anything, and a shell
 * that silently ate a field added on the renderer side would produce exactly the
 * "works in a browser tab, missing in the app" failure `safeEmulation`'s comment
 * warns about. What it may not carry is a value that is not JSON data.
 */
function safeTransferTab(raw) {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  if (!isViewId(raw.id)) return null;
  if (!PANE_KINDS.includes(raw.kind)) return null;
  let round;
  try {
    round = JSON.parse(JSON.stringify(raw));
  } catch {
    // Cyclic or otherwise unserializable — it could not have crossed IPC, but
    // the seed path stringifies again and a throw there loses the window.
    return null;
  }
  // `title` is the one field a previewed page controls directly
  // (`document.title` is pushed onto the tab record), so it is truncated rather
  // than allowed to set the size of everything downstream.
  if (typeof round.title === "string") round.title = round.title.slice(0, MAX_TITLE_LEN);
  if (utf8Length(JSON.stringify(round)) > MAX_TAB_BYTES) return null;
  return round;
}

/** A transfer's tabs, deduplicated by id and bounded. Two tabs sharing an id
 *  would fight over one shell in the window that receives them. */
function safeTransferTabs(raw) {
  if (!Array.isArray(raw)) return [];
  const seen = new Set();
  const out = [];
  for (const entry of raw) {
    const tab = safeTransferTab(entry);
    if (!tab || seen.has(tab.id)) continue;
    seen.add(tab.id);
    out.push(tab);
    if (out.length >= MAX_TRANSFER_TABS) break;
  }
  return out;
}

/**
 * The layout a detached window boots with, or `null` when there is nothing to
 * seed.
 *
 * Held by the main process and handed to the new renderer over the synchronous
 * `veld:window:seed` channel (`windows.js`) — **not** on a command line, which
 * is where it started and which was wrong on two counts; that history is in
 * `MAX_SEED_BYTES` above and at the channel itself.
 *
 * Built here rather than forwarded from the page, so the shell decides its shape
 * and its size. The result is read by `parseLayouts` in the new renderer, which
 * is what makes it a *layout* rather than a blob.
 */
function buildSeedLayout(worktreeId, tabs, ratio) {
  if (tabs.length === 0) return null;
  const r = Number(ratio);
  const seed = JSON.stringify({
    [worktreeId]: {
      docks: [
        { tabs, activeId: tabs[0].id },
        { tabs: [], activeId: null },
      ],
      ratio: Number.isFinite(r) ? r : 0.5,
      focused: 0,
    },
  });
  return utf8Length(seed) > MAX_SEED_BYTES ? null : seed;
}

/**
 * Recover a hand-back payload from a seed, for a detached window that closed
 * before its renderer ever reported one.
 *
 * The window is opened, the origin lets its tabs go, and then the window is
 * closed during the up-to-two-second daemon check — or while the waiting page is
 * up. There is no snapshot yet, so without this the tabs exist in no layout
 * anywhere and their shells die at the detach grace. The seed is exactly the set
 * that was handed over, which makes it the right thing to hand back.
 */
function transferFromSeed(seed) {
  if (typeof seed !== "string" || seed === "") return null;
  let parsed;
  try {
    parsed = JSON.parse(seed);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return null;
  for (const [key, layout] of Object.entries(parsed)) {
    const worktreeId = safeWorktreeId(key);
    if (worktreeId === null) continue;
    const docks = Array.isArray(layout?.docks) ? layout.docks : [];
    const tabs = safeTransferTabs(docks.flatMap((d) => (Array.isArray(d?.tabs) ? d.tabs : [])));
    if (tabs.length > 0) return { worktreeId, tabs };
  }
  return null;
}

module.exports = {
  ID_RE,
  PROFILE_RE,
  PANE_KINDS,
  MAX_SEED_BYTES,
  MAX_TAB_BYTES,
  MAX_TITLE_LEN,
  MAX_TRANSFER_TABS,
  safeUrl,
  isViewId,
  isProfileName,
  partitionFor,
  safeUserAgent,
  safeEmulation,
  safeZoom,
  safeColor,
  safeScale,
  safeRadius,
  safeTitle,
  safeWorktreeId,
  transferFromSeed,
  safeRepoRoot,
  safeTransferTab,
  safeTransferTabs,
  buildSeedLayout,
};
