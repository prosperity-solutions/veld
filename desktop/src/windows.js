// The app's windows: how many there are, which layout slot each owns, and what
// happens to a detached window's tabs when it closes.
//
// Veld Desktop has two kinds of window and they are the *same* window with
// different chrome (see desktop/ARCHITECTURE.md → "Windows"):
//
//   main      a full /ide — worktree rail, top bar, its own selection
//   detached  a bare dock holding tabs pulled out of another window
//
// One kind, not two, because both need everything that is actually hard here:
// a layout slot of their own, a place in the persisted window set, and the
// ownership rules below. The chrome is a query parameter.
//
// **A transfer moves a tab, it never copies one.** A layout names live PTY
// session ids and a second attach to a session *takes it over* rather than
// mirroring it, so two windows holding one tab id would trade the shell back and
// forth forever. Detach therefore removes the tab from the origin layout in the
// same step that seeds the new window, and closing a detached window hands its
// tabs *back* rather than ending them.

const { BrowserWindow, Notification, screen, shell } = require("electron");
const fs = require("node:fs");
const path = require("node:path");
const {
  buildSeedLayout,
  isViewId,
  safeRepoRoot,
  safeTitle,
  safeTransferTabs,
  safeTabId,
  safeTabIds,
  safeWorktreeId,
  transferFromSeed,
} = require("./validate");
const {

  canOpenAnother,
  cycleOrder,
  dropDelivery,

  handBackTarget,
  handBackTransfers,
  nextInCycle,
  nextListenerState,
  nextSuffix,

  ownsWorktree,
  parseWindowList,
  readLastMainBounds,
  releaseClaims: releaseClaimsIn,

  restoreBudget,
  safeBounds,
  trafficLightY,

  serializeWindowList,
  slotFor,
} = require("./windowState");

/**
 * @typedef {object} WindowRecord
 * @property {BrowserWindow} win
 * @property {string | null} suffix  `null` for the first window (bare base slot)
 * @property {string} slot
 * @property {"main" | "detached"} kind
 * @property {string | null} origin  which window a detached one came from, as a
 *   *suffix* — the only form that survives into `windows.json`
 * @property {number | null} originId  the same thing for this run, as a record
 *   id. Suffixes are recycled, so after `w2` closes and a new window takes the
 *   number, the persisted `origin: "w2"` names a window its tabs never came
 *   from; the record id never repeats and is what hand-back actually matches on.
 * @property {number} id  monotonic within this process
 * @property {string | null} seed  the layout this window boots with, served over
 *   `veld:window:seed` and retired when the renderer reports its **first
 *   snapshot** — not when it is read, and not when the page loads. Both of those
 *   were tried and both dropped it before anything else held the tabs; the
 *   channel's own docstring has the two failures.
 * @property {{worktreeId: number, tabs: object[]} | null} snapshot
 *   what this window would hand back if it closed now — pushed by the renderer
 *   on every layout change, so a hand-back does not depend on the renderer still
 *   being alive when `close` fires.
 * @property {{worktreeId: number, tabs: object[]}[]} pendingAdopt
 *   tabs handed to this window that its renderer has not collected yet
 * @property {boolean} closing  set on `close`, which is *before* `closed`: the
 *   record is still alive and matchable in that gap, and its adopt queue has
 *   already been handed on, so nothing may be given to it any more. Read by
 *   `queueDrop`, the drop target test, `handBack`'s target filter and
 *   `showsWorktree`'s pruning; `openSettings` and `focusPrimary` still match on
 *   `isDestroyed()` alone, which is pre-existing and outside this change.
 * @property {"unknown" | "ready" | "gone"} dropListener
 *   whether this window has a live listener for a cross-window drop. `unknown`
 *   until the renderer says — which an older `/ide` bundle never does, and which
 *   is why "unknown" is not "gone"; see `dropDelivery` in `windowState.js`.
 * @property {{worktreeId: number, ids: string[]} | null} tabs
 *   this window's tab strip, in the order it is drawn: dock 0 left-to-right,
 *   then dock 1. Pushed by **every** window's renderer on every layout change —
 *   unlike `snapshot`, which is detached-only and is a hand-back payload rather
 *   than an ordering.
 *
 *   The sole input to the cross-window tab cycle order. Kept here, in the only
 *   process that can see every window, rather than in a renderer's own registry
 *   of the windows it once opened: that registry cannot learn a window closed,
 *   cannot see a window it did not open, and has nowhere to record "the cycle is
 *   currently parked on somebody else" — all three of which were live bugs the
 *   first time this feature was attempted (#315).
 * @property {number | null} worktreeId
 * @property {string | null} repoRoot  which worktree a *detached* window is a
 *   dock for. Persisted and put back in its URL on restore: a bare dock has no
 *   rail, so without this it reopened against whatever the main window last
 *   selected and came back blank, with its real tabs unread in its own slot.
 * @property {number | null} zoomFactor  the last page zoom factor this window
 *   was synced for, so `syncZoom` can skip the work when it has not moved.
 *   `null` until the first sync.
 */

/** @type {Map<number, WindowRecord>} */
const windows = new Map();

/** Never reused, unlike a suffix. See `WindowRecord.originId`. */
let nextRecordId = 1;

/**
 * Which worktree each window is *displaying*: `worktreeId → record id`.
 *
 * **A routing hint, not an authority.** Ownership of a worktree — who may show
 * it, who has to let go, and what happens when you click one that is taken —
 * moved to the daemon (`crates/veld-daemon/src/ide.rs`), because this process
 * can only ever see its own windows and the same page also runs in a plain
 * browser tab. A tab was invisible to the arbitration that lived here, so it
 * opened worktrees the app already had and fought it for every shell.
 *
 * What is left is the one question only this process can answer: **when tabs are
 * dropped onto a window, is that a window this worktree's panes belong in?** The
 * renderer reports what it is showing (`veld:window:shows`) and `ownsWorktree`
 * reads it. A stale entry costs a drop that opens a new window instead of moving
 * tabs into an existing one — recoverable, and self-correcting on the next
 * report — where a stale *claim* used to grey out a rail row with no window
 * behind it.
 */
const showing = new Map();

// ---------------------------------------------------------------------------
// Cross-window tab drags
// ---------------------------------------------------------------------------

/**
 * A tab drag in flight, and the pointer it is following.
 *
 * **Drag events never leave the document they started in.** The window being
 * dragged *onto* is not told a drag exists, which is why — before this — it kept
 * its native views painting over any overlay, showed no insertion indicator, and
 * could only ever append a dropped tab at the end. Those were three faces of one
 * fact, not three bugs.
 *
 * So the shell carries the pointer across. It broadcasts the start (every window
 * freezes its browser views, because a `WebContentsView` paints over all DOM and
 * an overlay under one is invisible), then polls the cursor and hands it to
 * whichever window is under it, in *that window's* content coordinates. The
 * target then runs its ordinary drop code: same edge zones, same tab caret, same
 * preview. On release it commits what it was already showing.
 *
 * Polling rather than forwarding events, because the source stops receiving them
 * the moment the pointer leaves it — that is the whole problem. `browserViews.js`
 * forwards pointers window-wide for the same reason during a pane resize; this
 * is that idea one level up.
 */
let drag = null;
/**
 * Which window the most recent drag ended over, kept **after** the drag is over.
 *
 * Because the order the renderer reports things in must not matter. It ends the
 * drag (so every window thaws) and then asks where the tab should go, and a
 * first version that cleared this on the first call answered "nowhere" to the
 * second — so every cross-window drop opened a new window instead of moving the
 * tab, which looks exactly like the detached window reloading with its pane
 * still in it. Reset when the next drag starts, which is the only moment it
 * stops being the answer.
 */
let lastOverId = null;
const DRAG_POLL_MS = 16;

/**
 * Cross-window drops waiting for the receiving window to say it took them.
 *
 * **The source must not let go until the destination has them.** It releases
 * its terminals and closes the tabs on the strength of this handler's answer,
 * so answering "moved" the moment the message is *sent* means anything that
 * goes wrong on the far side — a renderer mid-reload, a payload its own
 * validation rejects, a worktree it no longer shows — loses the pane outright.
 * A tab that stayed put is a visible non-event; a tab that evaporated with a
 * live shell behind it is not recoverable by the user.
 *
 * A handler that answers resolves to the ids it placed, so "took nothing" and
 * "never answered" stay distinguishable: the first is a refusal to respect, and
 * the second (`null`, from the timeout or from the window closing) falls back to
 * the adopt queue rather than telling the user their pane could not be moved.
 */
const pendingDrops = new Map();
let nextDropId = 1;
const DROP_ACK_MS = 2000;

/** How many un-drained transfers may be parked on one window. A drop is queued
 *  only for a window whose renderer is not answering — loading, reloading, or
 *  wedged — so a queue this deep means it is not coming back, and the tabs are
 *  better off staying where they are than piling up somewhere nothing reads. */
const MAX_PENDING_ADOPT = 8;

/**
 * Whether a window currently has the app on screen at all — as opposed to nothing
 * yet, the `data:` waiting page, or an error page.
 *
 * **Compared as parsed origins, not as a string prefix.** `deps.baseUrl` comes
 * straight from `VELD_DESKTOP_URL` and is never normalised, while `getURL()` is
 * always Chromium's normalised form: `http://LOCALHOST:19899` lower-cases,
 * `http://127.0.0.1:80` drops its default port, and a trailing slash or a stray
 * `?`/`#` moves too. A prefix test fails on every one of those, and it fails
 * *silently* in the safe direction — every cross-window drop would be queued and
 * appended instead of placed at the caret, with nothing to indicate why. An
 * unparseable URL (`''` before the first load) is simply not the app.
 */
function showsApp(win) {
  try {
    return new URL(win.webContents.getURL()).origin === new URL(deps.baseUrl).origin;
  } catch {
    return false;
  }
}

/** Stop waiting on a window that is gone — `null`, the same answer the timeout
 *  gives, so the drop takes the same fallback without the two-second wait. */
function settleDropsFor(recordId) {
  for (const [dropId, pending] of [...pendingDrops]) {
    if (pending.targetId !== recordId) continue;
    pendingDrops.delete(dropId);
    pending.settle(null);
  }
}

/** How many worktrees one window may claim to hold. A window holds the ones it
 *  has visited, so this is generous; it exists so the map cannot grow without
 *  bound on a renderer's say-so. */
const MAX_HELD_WORKTREES = 256;

function beginDrag(sourceId) {
  endDrag();
  lastOverId = null;
  drag = { sourceId, overId: null, timer: null };
  for (const r of allRecords()) {
    if (!r.win.isDestroyed()) r.win.webContents.send("veld:window:drag-begin");
  }
  drag.timer = setInterval(pollDrag, DRAG_POLL_MS);
}

function pollDrag() {
  if (!drag) return;
  const point = screen.getCursorScreenPoint();
  // Bounds, and only bounds — Electron exposes no stacking order, so with two
  // *non-source* windows overlapping under the cursor this picks whichever was
  // created first rather than the one on top. Minimized and hidden windows are
  // excluded because their bounds are still their restore bounds, and a window
  // you cannot see swallowing the drop is the worst version of the ambiguity.
  // The remaining case — two visible windows overlapping at the pointer — is a
  // known limit rather than a solved problem.
  const over = allRecords().find((r) => {
    if (r.win.isDestroyed() || r.win.isMinimized() || !r.win.isVisible()) return false;
    const b = r.win.getBounds();
    return (
      point.x >= b.x && point.x <= b.x + b.width && point.y >= b.y && point.y <= b.y + b.height
    );
  });
  // Only the window *under* the cursor hears about it, and only it. The source
  // is included: while the pointer is back over it, its own DOM drag events are
  // doing the job and a forwarded position would fight them, so it is told the
  // pointer left instead.
  const overId = over && over.id !== drag.sourceId ? over.id : null;
  if (drag.overId !== null && drag.overId !== overId) {
    const previous = allRecords().find((r) => r.id === drag.overId);
    if (previous && !previous.win.isDestroyed()) {
      previous.win.webContents.send("veld:window:drag-out");
    }
  }
  drag.overId = overId;
  if (!over || overId === null) return;
  // Screen → that window's content coordinates, which is what its DOM works in.
  const area = over.win.getContentBounds();
  over.win.webContents.send("veld:window:drag-over", {
    x: point.x - area.x,
    y: point.y - area.y,
  });
}

function endDrag() {
  if (!drag) return;
  clearInterval(drag.timer);
  lastOverId = drag.overId;
  drag = null;
  for (const r of allRecords()) {
    if (!r.win.isDestroyed()) r.win.webContents.send("veld:window:drag-end");
  }
}

/** Forget windows that are gone, so a dead entry cannot route a drop. */
function pruneShowing() {
  const ids = new Set(allRecords().filter((r) => !r.win.isDestroyed()).map((r) => r.id));
  for (const [worktreeId, id] of showing) {
    if (!ids.has(id)) showing.delete(worktreeId);
  }
}

/** @type {null | {
 *   baseUrl: string,
 *   waitingHtml: string,
 *   daemonReachable: () => Promise<boolean>,
 *   appIcon: string,
 *   topbarHeight: number,
 *   trafficLightSize: number,
 *   slotBase: string,
 *   stateFile: string,
 *   disposeWindow: (win: BrowserWindow) => void,
 *   windowZoomChanged: (win: BrowserWindow, zoom: number) => void,
 * }} */
let deps = null;

/**
 * Set while the app is on its way out.
 *
 * A quit closes every window, and without this each detached one would hand its
 * tabs to a window that is also closing — pointless churn, and a write race on
 * the persisted window set that would record the app as having fewer windows
 * than it did. Quitting must leave the set exactly as it was, because that set
 * is what the next launch reopens.
 */
let quitting = false;

/**
 * Set it, and clear it only from somewhere that *knows* the quit is not
 * happening — never by inferring it from a window event.
 *
 * Two versions tried to infer it and both were wrong in opposite directions.
 * Re-arming on `browser-window-focus` fires mid-teardown, because closing the
 * front window hands key status to the next one. Filtering that with a
 * "windows closed since `before-quit`" counter only moves the question to
 * whether `closed` is emitted before that focus event — an ordering nobody
 * here can check without running the app, and getting it wrong restores the
 * first bug exactly.
 *
 * So the latch is one-way, and the *callers* that can prove the app is still
 * alive clear it: opening a window, `activate`, and the updater's install-failure
 * path — which is the only in-repo way a quit is actually cancelled
 * (`quitAndInstall` can return without quitting) and, being Linux's AppImage
 * updater, the one platform where `activate` never fires.
 */
function setQuitting(value) {
  quitting = value;
}

function windowCount() {
  return windows.size;
}

/** Every window, in the order they were created. */
function allRecords() {
  return [...windows.values()];
}

function recordFor(win) {
  return win ? (windows.get(win.id) ?? null) : null;
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

function readStateRaw() {
  try {
    return fs.readFileSync(deps.stateFile, "utf8");
  } catch {
    return "";
  }
}

/**
 * Write the current window set.
 *
 * Debounced, because it is driven by `move`/`resize`, which fire per frame while
 * a window is dragged. The trailing write is the one that matters; a lost
 * intermediate position is not a bug.
 */
let persistTimer = null;
/**
 * The bounds a fresh main window starts at, kept alive across a macOS close
 * that empties the window set. Updated from the live main window on every
 * persist, loaded from disk at init, and written back in `serializeWindowList`
 * — the one field in the state file that survives having no windows.
 */
let lastMainBounds = null;
/**
 * The bounds to persist for a window: always its live rect, never
 * `getNormalBounds()`.
 *
 * The latter looked like the right call — a window remembered while
 * maximised should restore to the size it had before, not to a literal
 * full-screen rect — until it was measured against a real resize: dragging a
 * window's edges to fill its display flips `isMaximized()` true on this
 * platform exactly the same as clicking the maximize control does, so
 * `getNormalBounds()` cannot tell "the user maximised this" from "the user
 * dragged it to nearly this size" apart — both land on the pre-drag restore
 * rect, which is stale in the second case. There is no reliable signal here
 * to recover the graceful-unmaximize behaviour with, so the simpler, correct
 * rule wins: what you see is what gets restored.
 */
function capturedBounds(win) {
  return win.getBounds();
}
function persistWindows() {
  // A quit closes every window one by one, and each `closed` would shrink the
  // recorded set — the last write winning would be "one window", which is what
  // the next launch would then reopen. The set as it stood before the quit is
  // already on disk (every create, move and close writes it), so the correct
  // behaviour during a quit is to write nothing at all.
  if (quitting) return;
  if (persistTimer) return;
  persistTimer = setTimeout(() => {
    persistTimer = null;
    // Re-checked, not only at scheduling time: a window moved and then quit
    // within the debounce leaves a timer that fires *during* the quit, by which
    // point some windows are already destroyed and would be filtered out of the
    // very list the next launch reopens.
    if (!deps || quitting) return;
    // The last live main window's bounds, captured *while it is still
    // alive* — the fallback bounds a fresh main window opens at. Captured here
    // rather than in `close` so it is current by the time the last window is
    // being torn down: when `closed` empties the set this has already been
    // updated by the resizes that led up to it, and writing it is what lets a
    // macOS red-X close followed by a reopen recall the size.
    const main = allRecords().find((r) => r.kind === "main" && !r.win.isDestroyed());
    if (main) lastMainBounds = safeBounds(capturedBounds(main.win));
    try {
      const records = allRecords()
        .filter((r) => !r.win.isDestroyed())
        .map((r) => ({
          suffix: r.suffix,
          kind: r.kind,
          origin: r.origin,
          worktreeId: r.worktreeId,
          repoRoot: r.repoRoot,
          // See `capturedBounds` for why this is the live rect, never
          // `getNormalBounds()`.
          bounds: safeBounds(capturedBounds(r.win)),
        }));
      fs.mkdirSync(path.dirname(deps.stateFile), { recursive: true });
      // Write-then-rename, because a torn file is worse than a stale one: a
      // truncated `windows.json` parses to `[]`, the next launch opens a single
      // window, and the first persist after that makes the loss of every other
      // window's layout — and the live shells its terminal ids name —
      // permanent.
      const tmp = `${deps.stateFile}.tmp`;
      fs.writeFileSync(
        tmp,
        serializeWindowList(readStateRaw(), deps.slotBase, records, undefined, lastMainBounds),
      );
      fs.renameSync(tmp, deps.stateFile);
    } catch {
      // An unwritable userData costs the window set on the next launch and
      // nothing else. The app still runs.
    }
  }, 400);
}

// ---------------------------------------------------------------------------
// Creating windows
// ---------------------------------------------------------------------------

function appUrl({ kind, repoRoot, worktreeId, settings }) {
  const params = new URLSearchParams({ shell: "electron" });
  // A detached window is one dock and nothing else. Unlike the layout slot this
  // is *fine* in the URL: it hides chrome and grants nothing, so a forged
  // `?chrome=none` in a browser tab is a page with no top bar, not access to
  // anything. The slot stays on the preload bridge for the opposite reason.
  if (kind === "detached") params.set("chrome", "none");
  if (repoRoot) params.set("repo", repoRoot);
  if (worktreeId) params.set("wt", String(worktreeId));
  // Only set when `⌘,` had no window to send to and one is being opened for it:
  // an IPC `send` would race the page load. Grants nothing (settings are a
  // daemon-side document either way), so a forged `?settings=1` in a browser tab
  // opens a dialog the user could have opened from the top bar's ⋯ menu.
  if (settings) params.set("settings", "1");
  return `${deps.baseUrl}/ide?${params.toString()}`;
}

async function loadAppWhenReady(win, url) {
  // `loadURL` rejects if the window is closed while it is in flight, which is a
  // normal thing for a user to do to a window that is waiting for a daemon. The
  // callers `void` this, so an uncaught one is an unhandled rejection per
  // window — noise that would bury a real failure.
  if (win.isDestroyed()) return;
  if (await deps.daemonReachable()) {
    if (win.isDestroyed()) return;
    await win.loadURL(url);
    return;
  }
  await win.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(deps.waitingHtml)}`);
  const timer = setInterval(async () => {
    if (win.isDestroyed()) {
      clearInterval(timer);
      return;
    }
    if (await deps.daemonReachable()) {
      clearInterval(timer);
      // Same treatment as the first load. This promise is discarded by the
      // interval, so without the catch a window closed across the
      // reachability check above — the ordinary case — is an unhandled
      // rejection, on the very path most likely to take it.
      if (win.isDestroyed()) return;
      await win.loadURL(url).catch((err) => {
        if (!win.isDestroyed()) console.error("[veld] window failed to load", err);
      });
    }
  }, 2000);
}

/**
 * Where a new detached window goes.
 *
 * Offset from the window it was pulled out of, on that window's display, rather
 * than centred: a detach is a gesture with a location, and a window that appears
 * in the middle of the primary display when you dragged a tab to the right-hand
 * monitor reads as the wrong window opening. Clamped to the work area so the
 * offset cannot push the title bar off-screen, which on macOS is unrecoverable
 * without the Window menu.
 */
function detachBounds(originWin) {
  const size = { width: 1000, height: 700 };
  const from = originWin && !originWin.isDestroyed() ? originWin.getNormalBounds() : null;
  const cursor = screen.getCursorScreenPoint();
  // Exactly 0,0 is treated as "no idea": it is a legal cursor position and also
  // what Wayland reports when it will not say, and being wrong about a real
  // top-left corner costs an offset window, while trusting Wayland's 0,0 would
  // send every detach to the primary display.
  const known = cursor.x !== 0 || cursor.y !== 0;
  const display = known
    ? screen.getDisplayNearestPoint(cursor)
    : from
      ? screen.getDisplayMatching(from)
      : screen.getPrimaryDisplay();
  const area = display.workArea;

  // **Where you dropped it**, on the display you dropped it on — which for this
  // gesture is the whole point: dragging a pane to a second monitor and having
  // the window appear back on the first is the one outcome that makes it
  // useless. The pointer lands just inside the new window's tab strip, the way
  // a tab torn out of a browser does, so the thing you were dragging is under
  // the cursor when it arrives rather than somewhere else on screen.
  //
  // Placing it by the *origin* window was the previous rule, and it was right
  // only while there was no trustworthy drop point to use instead.
  const x = known ? cursor.x - GRAB_X : from ? from.x + 48 : area.x + (area.width - size.width) / 2;
  const y = known ? cursor.y - GRAB_Y : from ? from.y + 48 : area.y + (area.height - size.height) / 2;

  const width = Math.min(size.width, area.width);
  const height = Math.min(size.height, area.height);
  return {
    width,
    height,
    // Clamped to the work area of the display it is going to, so the title bar
    // can never land off-screen — which on macOS is unrecoverable without the
    // Window menu.
    x: Math.round(Math.max(area.x, Math.min(x, area.x + area.width - width))),
    y: Math.round(Math.max(area.y, Math.min(y, area.y + area.height - height))),
  };
}

/** Where the pointer sits inside a freshly detached window: just inside its tab
 *  strip, so the pane you dragged is under the cursor when the window appears. */
const GRAB_X = 90;
const GRAB_Y = 18;

/** The suffixes currently spoken for, including the bare base as `null`. */
function takenSuffixes() {
  return new Set(allRecords().map((r) => r.suffix));
}

/**
 * Clamp remembered bounds to a display, so a fresh window cannot be restored
 * off-screen. A window remembered on a monitor that is gone (or on a smaller
 * one) would otherwise open with its title bar unreachable — unrecoverable on
 * macOS — so the position is moved onto whichever display the stored rect
 * overlaps most, and the size is capped to that display's work area.
 *
 * `null` in, `null` out: nothing remembered is the first-launch case, and the
 * caller falls back to the default size.
 */
function boundsOnScreen(bounds) {
  if (!bounds) return null;
  const area = screen.getDisplayMatching(bounds).workArea;
  const width = Math.min(bounds.width, area.width);
  const height = Math.min(bounds.height, area.height);
  return {
    width,
    height,
    // Clamped to the work area so the title bar can never land off-screen — the
    // same rule `detachBounds` applies for the same reason.
    x: Math.round(Math.max(area.x, Math.min(bounds.x, area.x + area.width - width))),
    y: Math.round(Math.max(area.y, Math.min(bounds.y, area.y + area.height - height))),
  };
}

/**
 * Bring everything that scales with page zoom into alignment with the current
 * factor: the traffic lights, the window's embedded browser views, and the page.
 *
 * The lights are OS-drawn at a fixed size while the top bar is CSS and scales
 * with the page's zoom factor, so as the page zooms the lights drift off the
 * bar's own controls — the drift `trafficLightY` exists to undo. The browser
 * views are the other half: a native view's bounds are DIP and the renderer
 * measures its panes in CSS pixels, so the factor is an input to every view's
 * geometry (`syncWindowZoom` in `browserViews.js`).
 *
 * **Driven by a poll, not an event, because Electron has no event that covers
 * every way the zoom can change.** `zoom-changed` fires only for the mouse
 * wheel; the View menu's `zoomIn`/`zoomOut`/`resetZoom` roles and their
 * `⌘+`/`⌘−`/`⌘0` accelerators change the factor with no event at all, and a
 * window restored on a per-origin remembered zoom never fires anything. A short
 * poll that acts when the factor *moved* is the one mechanism that catches all of
 * them. `getZoomFactor()` is a synchronous read of a cached value — a few hundred
 * per second across every window is noise next to the 16ms cursor poll this file
 * already runs.
 */
const ZOOM_POLL_MS = 100;
// The rate: Chromium animates wheel zoom over roughly a couple of frames, so
// 100ms repositions the lights mid-gesture instead of snapping at the end,
// while staying cheap enough to run for the lifetime of the window. There is
// nothing more to it than that — it is a latency/cheapness balance, not a
// measured value.
function syncZoom(record) {
  const win = record.win;
  if (win.isDestroyed()) return;
  const zoom = win.webContents.getZoomFactor();
  if (zoom === record.zoomFactor) return;
  record.zoomFactor = zoom;
  // `setWindowButtonPosition` exists on every platform but only does anything
  // where `titleBarStyle: "hiddenInset"` is honoured — macOS. The `typeof`
  // guard keeps a hypothetical Electron that drops the method from taking the
  // window down with it; the position is cosmetic on the other platforms anyway.
  //
  // **Guarded here rather than at the top of the function**, which is where it
  // used to be: everything below is platform-independent, and gating the whole
  // sync on a macOS-only cosmetic call left a zoomed /ide on Windows and Linux
  // with mispositioned native views and a top-bar inset that never moved.
  //
  // **A detached window is excluded, and that is not the same guard.** It keeps a
  // normal frame — no `titleBarStyle`, no `trafficLightPosition` (see
  // `openWindow`) — so it has no custom traffic lights to reposition, and the
  // method is documented only for a window whose title bar is hidden. This
  // function now runs for detached windows (they hold browser panes, whose
  // geometry the factor drives), which is exactly what makes the distinction
  // load-bearing rather than incidental.
  if (record.kind !== "detached" && typeof win.setWindowButtonPosition === "function") {
    win.setWindowButtonPosition({
      x: 13,
      y: trafficLightY(deps.topbarHeight, deps.trafficLightSize, zoom),
    });
  }
  // The embedded browser views before the page: this re-places views the page has
  // already told us about, and a page that re-measures in response to the zoom
  // step lands on top of it with the same numbers.
  deps.windowZoomChanged(win, zoom);
  // The page's CSS keeps the traffic-light inset — the gap before the first
  // control, the view switcher — fixed in DIP as the page zooms, so it needs to
  // know the factor. See `watchZoom` in the UI and `--topbar-zoom` in
  // `styles.css`. Sent only when the factor moved, which is exactly when the
  // CSS inset has to move to hold its DIP width.
  win.webContents.send("veld:window:zoom", { zoom });
}

/**
 * Open a window.
 *
 * `suffix === undefined` allocates the next free one; pass an explicit suffix
 * (including `null`) to reopen a persisted window on the slot it had, which is
 * what makes its terminals reachable again after an app restart.
 *
 * @returns {BrowserWindow | null} `null` when the window cap is reached.
 */
function openWindow(options = {}) {
  const { kind = "main", origin = null, seed = null, repoRoot = null, worktreeId = null } = options;
  // Opening a window means we are not on our way out after all. `before-quit`
  // can fire without a quit following it — anything may cancel one — and a flag
  // that only ever moves in one direction would silently disable window
  // persistence for the rest of the session.
  quitting = false;
  const explicit = Object.hasOwn(options, "suffix");
  if (!explicit && !canOpenAnother(windows.size)) return null;

  const suffix = explicit ? options.suffix : nextSuffix(takenSuffixes());
  if (!explicit && suffix === null) return null;
  const slot = slotFor(deps.slotBase, suffix);
  const detached = kind === "detached";

  const bounds =
    safeBounds(options.bounds) ??
    (detached
      ? detachBounds(options.originWindow ?? null)
      : boundsOnScreen(lastMainBounds) ?? { width: 1280, height: 800 });

  const win = new BrowserWindow({
    // Titled by the app for a main window (the page's <title> is the daemon
    // bundle's, which says nothing about which window this is). A detached
    // window is the opposite case: it holds one dock, so the active tab's title
    // *is* the most useful thing a title bar could say, and the renderer sets it.
    title: "Veld",
    ...bounds,
    minWidth: detached ? 420 : 900,
    minHeight: detached ? 300 : 540,
    // A detached window keeps a normal frame. The main window's UI draws veld
    // controls into the title-bar row and so has to own it; a bare dock has no
    // such row, and a frameless window with nothing to drag by is a window you
    // cannot move.
    ...(detached
      ? {}
      : {
          titleBarStyle: "hiddenInset",
          // The creation-time position assumes the page starts at 100% zoom;
          // `syncZoom` (below) repositions it once the page reports
          // its real factor — a window restored on a remembered zoom sits wrong
          // for at most a poll tick.
          trafficLightPosition: {
            x: 13,
            y: trafficLightY(deps.topbarHeight, deps.trafficLightSize, 1),
          },
        }),
    backgroundColor: "#0d0e10",
    icon: deps.appIcon,
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      preload: path.join(__dirname, "preload.js"),
      // How the preload learns what this window is. Not query parameters: the
      // renderer keys durable state naming live PTY sessions off the slot, and
      // the seed *is* such state — a link can forge a URL but not a preload
      // argument. `chrome=none` is in the URL precisely because it is the one
      // piece here that grants nothing.
      // The slot and the kind are short, fixed-charset and not secret, so argv
      // is the right home: a link can forge a URL but not a preload argument.
      // The **seed is deliberately not here** — see `record.seed` and the
      // `veld:window:seed` channel below.
      additionalArguments: [
        `--veld-layout-slot=${slot}`,
        `--veld-window-kind=${kind}`,
        // An explicit suffix means the caller is *reopening* a window on a slot
        // it owned before (`restoreWindows`, or `focusPrimary` bringing the app
        // back with no windows left). Allocating one means a genuinely new
        // window, which must not adopt whatever layout the recycled number's key
        // still holds — that layout names terminal ids another window may be
        // attached to, and attaching takes them over.
        `--veld-window-restored=${explicit ? "1" : "0"}`,
      ],
    },
  });

  /** @type {WindowRecord} */
  const record = {
    win,
    suffix,
    slot,
    kind,
    origin,
    originId: options.originId ?? null,
    id: nextRecordId++,
    // Only meaningful for a detached window; see `parseWindowRecord`.
    worktreeId,
    repoRoot,
    seed,
    snapshot: null,
    tabs: null,
    pendingAdopt: [],
    dropListener: "unknown",
    closing: false,
    // Set by the first `syncZoom`; null means "not yet synced", so the first
    // poll after creation always runs — which matters when the page loaded at a
    // remembered non-100% zoom.
    zoomFactor: null,
  };
  windows.set(win.id, record);

  // A page navigating away takes its drop listener with it, and the renderer
  // gets no chance to say so — an unload handler is not a place to await an IPC
  // round trip. So the shell notices instead. Which navigations count, and why
  // `unknown` is not demoted, is `nextListenerState` in `windowState.js`, where
  // it is a decision over plain values and therefore has tests.
  //
  // Nothing here touches worktree ownership any more: the daemon holds a
  // client's claims for as long as its control socket is open, and a reload
  // closes that socket. It also has a short grace in which the same client id
  // reconnecting takes its claims back, which is a thing this process could
  // never have done — it cannot tell a reload from a close.
  win.webContents.on("did-start-navigation", (details) => {
    record.dropListener = nextListenerState(record.dropListener, details);
    // **And the tab strip, for exactly the reason stated above.** A page
    // navigating away takes its strip with it and gets no chance to say so, so
    // without this the record keeps advertising tabs the window no longer draws
    // — and cycling then raises that window and asks a renderer that isn't
    // there to activate one, which reads as "the chord stole my focus and did
    // nothing". A reload that fails outright (the daemon stopped, `veld update`
    // mid-restart) leaves the stale strip there indefinitely.
    //
    // **`isSameDocument` matters as much as `isMainFrame`** — the same pair
    // `nextListenerState` filters on, one line above, and for a sharper reason
    // here. `/ide` calls `history.replaceState` on every worktree selection and
    // every IDE/Runs switch, which fires this event without replacing anything;
    // clearing on it drops the window from the cycle order *permanently*,
    // because the renderer's push is deduped on the tab ids and nothing about
    // them changed, so it never re-sends. A single rail click would have killed
    // cycling for the rest of the session.
    if (details.isMainFrame && !details.isSameDocument) record.tabs = null;
  });

  // Run URLs open in the user's real browser, never inside the shell.
  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: "deny" };
  });

  const url = appUrl({ kind, repoRoot, worktreeId, settings: options.settings });
  const appOrigin = new URL(url).origin;
  win.webContents.on("will-navigate", (event, target) => {
    // Fail CLOSED: an unparseable target must not fall through into the shell
    // (skipping preventDefault would navigate).
    let targetOrigin = null;
    try {
      targetOrigin = new URL(target).origin;
    } catch {
      // leave null → blocked below
    }
    if (targetOrigin !== appOrigin) {
      event.preventDefault();
      if (targetOrigin) void shell.openExternal(target);
    }
  });

  if (!detached) {
    // A detached window's title belongs to the page, which sets it from the
    // active tab — so only a main window cancels this.
    // Electron adopts `document.title` on every navigation unless the event is
    // cancelled, which is how a hard reload renamed the window.
    win.on("page-title-updated", (e) => e.preventDefault());
  }

  win.on("move", persistWindows);
  win.on("resize", persistWindows);

  // Page zoom is the opposite of full screen in one sense — the lights stay in
  // the content area, but the top bar that anchors them grows and shrinks with
  // the page's zoom factor, so they drift off it. `syncZoom` follows the factor
  // and repositions them. See `ZOOM_POLL_MS` for why this is a poll rather than
  // an event, and why the `did-finish-load` sync is a backstop for the
  // per-origin zoom a window can be restored at.
  //
  // **Every window, detached included.** This used to be `if (!detached)`, on the
  // reasoning that a detached window keeps a normal frame and has no traffic
  // lights to move — true, and no longer the only thing the factor drives: a
  // detached window is a bare dock holding tabs pulled out of another one, so it
  // holds browser panes, whose native views are placed in DIP from a box the page
  // measures in CSS pixels.
  const zoomTimer = setInterval(() => syncZoom(record), ZOOM_POLL_MS);
  win.on("closed", () => clearInterval(zoomTimer));
  win.webContents.on("did-finish-load", () => syncZoom(record));

  // Full screen is main-process knowledge: macOS moves the traffic lights out of
  // the content area, and the page's top bar has to give back the inset it holds
  // for them. Nothing in the DOM can see this — `:fullscreen` is the element API
  // and `display-mode: fullscreen` does not match an Electron window — so the
  // shell is the only thing that can tell it. The page reads the state it booted
  // into over `veld:window:fullscreen` (below) and hears about changes here.
  const sendFullScreen = (fullScreen) => {
    if (win.isDestroyed()) return;
    win.webContents.send("veld:window:fullscreen", { fullScreen });
  };
  win.on("enter-full-screen", () => sendFullScreen(true));
  win.on("leave-full-screen", () => sendFullScreen(false));

  win.on("close", () => {
    // **Nothing may be given to this window from here on.** `handBack` below drains
    // its queue, while the record stays alive and matchable until `closed` — so a
    // drop resolving in that gap would park tabs in a queue that has already been
    // handed on and will never be drained or carried again, *after* the source let
    // go of them on a `moved: true`. A vanished pane with a live shell behind it is
    // the one outcome this protocol exists to prevent, so the flag is set before
    // anything else here.
    record.closing = true;
    // Before `closed`: the window's `contentView` must still exist to detach the
    // browser panes from, and a view outliving its window keeps a renderer
    // process alive with nothing to paint into.
    deps.disposeWindow(win);
    handBack(record);
  });
  win.on("closed", () => {
    // Otherwise the 16ms poll runs forever against a dead source, every other
    // window stays view-frozen with no visible cause, and they keep rendering
    // drop carets for a gesture that ended.
    if (drag?.sourceId === record.id) endDrag();
    // Anything still waiting on this window has its answer: it is not going to
    // place a drop. It falls back on its own, but only after a timeout the user
    // would feel as a stalled gesture.
    settleDropsFor(record.id);
    releaseClaimsIn(showing, record.id);
    windows.delete(win.id);
    persistWindows();
    // Its worktrees becoming pickable again is the daemon's to notice — the
    // page's control socket died with the window, which is what releases them.
  });

  // A window opened while a drag is in flight is polled and asked to resolve a
  // target like any other, so it has to freeze its views like any other.
  if (drag) win.webContents.on("did-finish-load", () => {
    if (!win.isDestroyed() && drag) win.webContents.send("veld:window:drag-begin");
  });

  void loadAppWhenReady(win, url).catch((err) => {
    // A window closed mid-load is the ordinary case and says nothing. Anything
    // else is a window that will sit there blank, and before this was a promise
    // it surfaced as an unhandled rejection — swallowing every failure alike
    // would remove the only signal that reached anyone.
    if (!win.isDestroyed()) console.error("[veld] window failed to load", err);
  });
  persistWindows();
  return win;
}

/**
 * Give a closing detached window's tabs to another window.
 *
 * Closing a *pane* ends its shell, and must keep meaning that. Closing a
 * detached *window* must not: the tabs in it were pulled out of somewhere, and
 * the window is a place to put them, not a lifetime. So they go back — to the
 * window they came from when it is still open, and otherwise to any main window,
 * because the alternative is a shell that is alive, unreachable, and hung up
 * thirty minutes later by the detach grace.
 *
 * Nothing here ends a session on the failure paths either: an un-adopted tab
 * leaves its shell running under the grace, which is recoverable, while a
 * hang-up is not.
 */
function handBack(record) {
  if (quitting) return;
  // **A queue is a resting place, never a grave.** Anything handed to this window
  // that its renderer never collected is carried on, whatever kind of window this
  // is: a drop routed at a loading window parks here (see `queueDrop`), and the
  // source has already let go of those tabs on the strength of that. Closing the
  // window before it drained would otherwise end exactly the shells the ack
  // protocol exists to protect.
  const carried = record.pendingAdopt.splice(0);
  // The seed is the fallback for a window that never got far enough to report a
  // snapshot — closed during the daemon check, or while the waiting page was up.
  // Its tabs were released by the origin the moment the detach was accepted, so
  // without this they exist in no layout anywhere and die at the grace.
  const own = record.kind === "detached" ? (record.snapshot ?? transferFromSeed(record.seed)) : null;
  // Composition — queue first, and which sources a *main* window contributes — is
  // `handBackTransfers` in `windowState.js`, where it has tests.
  const transfers = handBackTransfers(record.kind, carried, own);
  if (transfers.length === 0) return;

  // The precedence — record id, then persisted suffix, then any main window —
  // is `handBackTarget` in `windowState.js`, where it is a decision over plain
  // records and therefore has tests.
  // `closing` as well as destroyed, for the reason `queueDrop` checks it: a window
  // past its own `close` has *already* drained its queue in its own `handBack`, so
  // handing to it puts these tabs somewhere nothing will drain or carry again —
  // "a queue is a resting place, never a grave" failing by the sibling path. Two
  // windows closing in quick succession is all it takes. With no eligible target
  // these shells outlive the app under the detach grace, which is the documented
  // fallback and is recoverable; a discarded queue is not.
  const others = allRecords().filter((r) => r !== record && !r.win.isDestroyed() && !r.closing);
  const target = handBackTarget(record, others);
  if (!target) return;

  // **Queued, then nudged** — never sent as a payload. `webContents.send` is
  // fire-and-forget, and the listener on the other end only exists once the
  // `/ide` bundle has mounted: a hand-back to a window still on the waiting page
  // (the daemon is restarting — precisely the case terminals are built to
  // survive), mid-reload, or still being restored would be dropped on the floor,
  // and the tabs would be gone despite the docs promising they come back. The
  // renderer collects this queue at mount *and* on the nudge, so neither
  // ordering loses it.
  // **Past `MAX_PENDING_ADOPT` on purpose, unlike `queueDrop`.** That cap can
  // refuse because refusing leaves the tabs where they are; here there is no
  // "where they are" — this window is closing and these tabs are in no layout
  // anywhere. Dropping them to respect a bound would end the shells the cap was
  // protecting, so the queue is allowed to overshoot on the way out.
  target.pendingAdopt.push(...transfers);
  target.win.webContents.send("veld:window:adopt");
}

/**
 * Park a cross-window drop for a window that cannot answer for it yet.
 *
 * The precedent is `handBack`'s: `webContents.send` is fire-and-forget and the
 * listener arrives late, so the tabs go in the queue the renderer drains at mount
 * and on the nudge, rather than at a listener that does not exist. What is lost
 * is the *position* — a window with no mounted `PaneArea` previewed nothing, so
 * there is no caret or edge to honour and these are appended, which is what the
 * queue can express.
 *
 * This is the one path where the main process is the custodian of tabs the source
 * has let go of, which is why `handBack` carries an un-drained queue on rather
 * than dropping it. A window that is *gone* is a different answer: refuse, and
 * the tabs stay where they are — the failure mode the whole protocol prefers.
 */
function queueDrop(target, worktreeId, tabs) {
  // `closing` as well as destroyed, and it is not belt-and-braces: a window between
  // `close` and `closed` is alive, still in `allRecords()`, and has already had its
  // queue handed on. The async path is why this is re-checked here rather than only
  // at the call site — `target` was resolved before a `DROP_ACK_MS` wait.
  if (target.win.isDestroyed() || target.closing) {
    return { moved: false, opened: false, reason: "refused" };
  }
  if (target.pendingAdopt.length >= MAX_PENDING_ADOPT) {
    return { moved: false, opened: false, reason: "refused" };
  }
  target.pendingAdopt.push({ worktreeId, tabs });
  target.win.webContents.send("veld:window:adopt");
  // Raised exactly as an accepted drop raises it: the user dropped *on* this
  // window, and a window that takes the tabs and stays behind another one reads
  // as nothing having happened.
  if (target.win.isMinimized()) target.win.restore();
  target.win.show();
  target.win.focus();
  return { moved: true, opened: false, accepted: tabs.map((t) => t.id) };
}

/**
 * Ask a window with chrome to open the settings surface.
 *
 * `⌘,` is a *menu accelerator*, so it fires on whatever is in front — including a
 * chrome-less detached window, which renders one pane and has no top bar and no
 * dismissal context. A modal there would cover the only thing that window exists
 * to show. So the accelerator is routed to the focused window when it has chrome,
 * and otherwise to a main window (opening one if none is left), which is raised.
 *
 * The cost is stated rather than hidden: this bends the OS convention that a menu
 * accelerator acts on the front window. The alternative is a key that silently
 * does nothing in a detached window, and #201 already established that a shortcut
 * which quietly no-ops is worse than one that moves you somewhere it works.
 */
function openSettings() {
  const focused = BrowserWindow.getFocusedWindow();
  const record = focused ? recordFor(focused) : null;
  if (record && record.kind === "main" && !record.win.isDestroyed()) {
    record.win.webContents.send("veld:app:settings");
    return;
  }
  const target = allRecords().find(
    (r) => r.kind === "main" && !r.win.isDestroyed(),
  );
  if (!target) {
    // No main window left — open one that opens settings itself on boot via the
    // query flag, since a `send` would race the page load.
    //
    // Deliberately NOT `focusPrimary`: its second fallback matches *any* live
    // window, so with only a chrome-less detached window alive it would focus that
    // and return, dropping the flag — ⌘, would silently do nothing, which is the
    // exact outcome this function exists to prevent. Caught in review.
    if (takenSuffixes().has(null)) openWindow({ kind: "main", settings: true });
    else openWindow({ kind: "main", suffix: null, settings: true });
    return;
  }
  if (target.win.isMinimized()) target.win.restore();
  target.win.focus();
  target.win.webContents.send("veld:app:settings");
}

/**
 * Hand a File-menu tab command (`"new"` / `"close"`) to the focused window.
 *
 * These are **menu accelerators rather than key handlers in the page**, for the
 * same reason ⌘, is one: a focused `WebContentsView` swallows every keystroke, so
 * a browser pane would be the one place in the app where ⌘W did nothing. A menu
 * accelerator is handled before web contents see the key, so it needs no
 * per-pane forwarding at all.
 *
 * Unlike `openSettings`, there is **no fallback to some other window**. A tab
 * command is about the strip in front of you; running it against a different
 * window because this one cannot answer would close a tab somewhere the user is
 * not looking. Nothing focused, or a window with no page to tell, means the
 * chord does nothing — which is what an accelerator over a native menu or a
 * window being torn down should do.
 */
function tabCommand(command) {
  const focused = BrowserWindow.getFocusedWindow();
  const record = focused ? recordFor(focused) : null;
  if (!record || record.closing || record.win.isDestroyed()) return;
  record.win.webContents.send("veld:app:tab-command", { command });
}

/** Focus a main window, opening one if every window is gone (macOS keeps the
 *  app alive with no windows). */
function focusPrimary() {
  const target =
    allRecords().find((r) => r.kind === "main" && !r.win.isDestroyed()) ??
    allRecords().find((r) => !r.win.isDestroyed()) ??
    null;
  if (!target) {
    // Allocate rather than assuming the bare base is free: a record whose window
    // is destroyed is removed on `closed`, and there is no ordering guarantee
    // between that and the `activate` this runs from. Two windows on one slot is
    // the one outcome worth a branch to avoid.
    if (takenSuffixes().has(null)) openWindow({ kind: "main" });
    else openWindow({ kind: "main", suffix: null });
    return;
  }
  if (target.win.isMinimized()) target.win.restore();
  target.win.show();
  target.win.focus();
}

/**
 * Reopen the windows the last run ended with.
 *
 * The window set is persisted for the same reason a layout is: a detached window
 * holds live shells, and a relaunch that opens only the main window leaves them
 * running, unreachable, until the grace hangs them up. Always at least one
 * window — a first launch has no file, and a file that lists nothing usable is
 * the same situation.
 */
function restoreWindows() {
  const stored = parseWindowList(readStateRaw(), deps.slotBase);
  // Room for the fallback below is reserved only when it will actually be
  // needed, which is knowable up front — see `restoreBudget`.
  const budget = restoreBudget(stored);
  const opened = [];
  for (const entry of stored) {
    if (windows.size >= budget) break;
    const win = openWindow({
      kind: entry.kind,
      suffix: entry.suffix,
      origin: entry.origin,
      bounds: entry.bounds,
      repoRoot: entry.repoRoot,
      worktreeId: entry.worktreeId,
    });
    if (win) opened.push(win);
  }

  // Resolve every persisted `origin` suffix to the record id it now refers to,
  // once, here. After this the suffix branch of `handBackTarget` never runs
  // again — and it must not, because from the first ⌘N onward a freed suffix can
  // belong to a window these tabs never came from. Suffixes are unique across
  // the set that was just restored, so this is the one moment the mapping is
  // unambiguous.
  const bySuffix = new Map(allRecords().map((r) => [r.suffix, r.id]));
  for (const record of allRecords()) {
    if (record.originId === null && record.origin !== null) {
      record.originId = bySuffix.get(record.origin) ?? null;
    }
  }
  if (!opened.some((w) => recordFor(w)?.kind === "main")) {
    // Either nothing was stored, or everything stored was a detached window
    // (its origin closed last). Either way the app needs a window with a rail in
    // it, and the bare base slot is where the previous main window's layout is —
    // unless a restored window already took it, in which case allocate.
    //
    // Spelled as two calls rather than one with a conditional `suffix`, because
    // `Object.hasOwn` is true for a key whose value is `undefined`: passing
    // `{suffix: undefined}` reads as "reopen on the bare base" and would put two
    // windows on one slot, which is the collision slots exist to prevent.
    if (takenSuffixes().has(null)) openWindow({ kind: "main" });
    else openWindow({ kind: "main", suffix: null });
  }
}

// ---------------------------------------------------------------------------
// IPC
// ---------------------------------------------------------------------------

/**
 * Resolve the window that sent a request.
 *
 * Main-frame only, mirroring `browserViews.js`: an iframe inside a pane must not
 * be able to open windows or move another window's tabs around.
 */
function senderWindow(event) {
  const win = BrowserWindow.fromWebContents(event.sender);
  if (!win || win.isDestroyed()) return null;
  if (event.senderFrame !== win.webContents.mainFrame) return null;
  return win;
}

function registerWindowIpc(ipcMain) {
  /**
   * The layout a freshly detached window boots with.
   *
   * **Synchronous, and read from the preload.** It has to be available in the
   * renderer's *first* render: `pruneTerminals` ends every session the layouts
   * do not name, so a seed arriving one tick late reads as "these are orphans"
   * and hangs up the shells that were just transferred.
   *
   * It used to ride `additionalArguments`, which was wrong twice. A process's
   * argv is world-readable on Linux (`/proc/<pid>/cmdline` is 0444), and a seed
   * carries browser panes' URLs — query strings and fragments included, which is
   * where an implicit-flow access token lives. And the size ceiling guarded the
   * wrong number: the JSON's UTF-16 length, while what had to fit was the base64
   * of its UTF-8 bytes, up to 4× larger and past Linux's 128 KB-per-argument
   * limit — so a page with a very long title produced a window whose renderer
   * never started, after the origin had already let its tabs go.
   *
   * `ipcMain.on` + `event.returnValue` rather than `handle`, because `handle` is
   * a promise and this must not be one.
   *
   * **Cleared when the renderer first reports a snapshot, not when it is read
   * and not when the page loads.** Two versions of this were wrong, both in the
   * same direction — retiring the seed before anything else held the layout:
   *
   *  - Read-once: a preload runs on *every* load in a `webContents`, the `data:`
   *    waiting page included, so the seed was consumed by that page and gone by
   *    the time `/ide` loaded. Detaching during a daemon restart is the case
   *    terminals exist to survive, and it was the case that lost them.
   *  - Cleared at `did-finish-load`: a loaded page still does not know its
   *    layout — the renderer cannot report one until `/api/repos` resolves the
   *    worktree, and a failed first request retries five seconds later.
   *
   * A snapshot is the proof that something else now holds these tabs. Until one
   * arrives, `handBack` falls back to the seed. Re-reading it is harmless: by
   * then the page's own storages win over it (see `readLayouts`).
   *
   * Resolved from `event.sender` alone. Electron runs a preload in the main
   * frame only (`nodeIntegrationInSubFrames` is off), and the embedded panes have
   * no preload at all, so nothing else can reach this channel — while
   * `event.senderFrame` is not reliably populated this early, which is the one
   * place the main-frame check every other handler makes would fail closed on
   * the legitimate caller.
   */
  ipcMain.on("veld:window:seed", (event) => {
    const win = BrowserWindow.fromWebContents(event.sender);
    event.returnValue = recordFor(win)?.seed ?? null;
  });

  /**
   * The page's zoom factor right now.
   *
   * Synchronous, and read from the preload, for the same reason the seed is:
   * the top bar's traffic-light inset is `calc(100px / var(--topbar-zoom))`, so
   * a zoom arriving a tick late shows the view switcher at the wrong distance
   * from the lights on the first paint of a window restored at a remembered
   * zoom. Changes arrive on `veld:window:zoom` (pushed by `syncZoom`),
   * which is the only time the factor has moved.
   */
  ipcMain.on("veld:window:zoom", (event) => {
    const win = BrowserWindow.fromWebContents(event.sender);
    event.returnValue = win && !win.isDestroyed() ? win.webContents.getZoomFactor() : 1;
  });

  /**
   * Whether this window is in native full screen *right now*.
   *
   * Synchronous, and read from the preload, for the same reason the seed is: it
   * has to be true in the renderer's first paint. The top bar reserves 90px for
   * the traffic lights, full screen takes those lights away, and an answer
   * arriving a tick later means every reload in full screen — and every full
   * screen the app is *relaunched* into — flashes an empty gutter before the bar
   * snaps left.
   *
   * Resolved from `event.sender` alone, like the seed: `event.senderFrame` is not
   * reliably populated at preload time, and only the main frame gets a preload.
   */
  ipcMain.on("veld:window:fullscreen", (event) => {
    const win = BrowserWindow.fromWebContents(event.sender);
    event.returnValue = win && !win.isDestroyed() ? win.isFullScreen() : false;
  });

  /**
   * Open another full window, optionally already pointed at a worktree.
   *
   * The selection travels as a payload rather than being left to the new
   * window's own persisted key: that key is per slot, and a brand-new slot has
   * nothing in it, so the window would open on whatever was last selected
   * app-wide. `⌘N` sends nothing and gets exactly that fallback, which is the
   * right answer for "another window like this one"; the rail's *Open in a new
   * window* sends the worktree you right-clicked.
   */
  ipcMain.handle("veld:window:new", (event, payload) => {
    if (!senderWindow(event)) return { opened: false, reason: "no-window" };
    if (!canOpenAnother(windows.size)) return { opened: false, reason: "cap" };
    const win = openWindow({
      kind: "main",
      repoRoot: safeRepoRoot(payload?.repoRoot),
      worktreeId: safeWorktreeId(payload?.worktreeId),
    });
    return { opened: win !== null, reason: win ? null : "cap" };
  });

  /**
   * Which worktree this window is displaying.
   *
   * **Reporting, not asking.** Whether a window *may* show a worktree is the
   * daemon's answer now (`crates/veld-daemon/src/ide.rs`); this is the page
   * telling the shell what it settled on, and the only consumer is drop routing
   * — `ownsWorktree`, which has to know whether a window the pointer is over is
   * a place this worktree's tabs belong.
   *
   * A detached window never reports: its `worktreeId` is fixed at creation and
   * is what `ownsWorktree` reads for it.
   */
  ipcMain.handle("veld:window:shows", (event, payload) => {
    const record = recordFor(senderWindow(event));
    if (!record || record.kind !== "main") return false;
    releaseClaimsIn(showing, record.id);
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    if (worktreeId !== null) showing.set(worktreeId, record.id);
    pruneShowing();
    return true;
  });

  /**
   * Ask to show a worktree — **for an `/ide` bundle older than this shell only.**
   *
   * A current bundle asks the daemon, which arbitrates across browser tabs too.
   * This exists because that older bundle keeps its main-window layouts in one
   * `localStorage` key shared between windows, so without an answer here two
   * windows render one worktree's panes and trade every shell in it. Answering a
   * blanket yes would have been worse than not stubbing at all.
   *
   * `showing` is the same map drop routing uses; the older bundle populates it
   * through this call rather than through `veld:window:shows`, which it does not
   * know about. No yield handshake — see the preload comment.
   */
  ipcMain.handle("veld:window:legacy-claim", (event, payload) => {
    const record = recordFor(senderWindow(event));
    if (!record) return { ok: false, reason: "no-window" };
    if (record.kind !== "main") return { ok: true };
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    if (worktreeId === null) return { ok: false, reason: "invalid" };
    pruneShowing();
    const holderId = showing.get(worktreeId);
    if (holderId !== undefined && holderId !== record.id) {
      const holder = allRecords().find(
        (r) => r.id === holderId && !r.win.isDestroyed() && !r.closing,
      );
      if (holder) {
        // Only a deliberate pick raises the other window; a window working out
        // what it may display asks about several in a row.
        if (payload?.focusHolder !== false) {
          if (holder.win.isMinimized()) holder.win.restore();
          holder.win.show();
          holder.win.focus();
        }
        return { ok: false, reason: "shown-elsewhere" };
      }
      showing.delete(worktreeId);
    }
    releaseClaimsIn(showing, record.id);
    showing.set(worktreeId, record.id);
    return { ok: true };
  });

  /** Which worktrees another window of this app is showing — older bundles. */
  ipcMain.handle("veld:window:legacy-elsewhere", (event) => {
    const record = recordFor(senderWindow(event));
    if (!record) return [];
    pruneShowing();
    const ids = [];
    for (const [worktreeId, id] of showing) {
      if (id !== record.id) ids.push(worktreeId);
    }
    return ids;
  });

  /**
   * Bring this window to the front.
   *
   * The daemon decides *who* is wanted — somebody clicked a worktree this window
   * is showing — and this is the part only the shell can do. Deliberately not
   * gated on which window asks: the page asking is the page the daemon told to
   * raise itself, and a window raising itself is not a capability worth
   * defending. What it replaces is the shell reaching into another window on a
   * refusal it computed itself, which a browser tab could never take part in.
   */
  ipcMain.handle("veld:window:focus-self", (event) => {
    const win = senderWindow(event);
    if (!win || win.isDestroyed()) return false;
    if (win.isMinimized()) win.restore();
    win.show();
    win.focus();
    return true;
  });

  /** A tab drag started here. Every window freezes its views; the cursor starts
   *  being carried to whichever one it is over. */
  ipcMain.handle("veld:window:drag-begin", (event) => {
    const record = recordFor(senderWindow(event));
    if (!record) return false;
    beginDrag(record.id);
    return true;
  });

  /** …and ended, however it ended. Idempotent: `drop-out` ends it too. */
  ipcMain.handle("veld:window:drag-end", (event) => {
    if (!senderWindow(event)) return false;
    endDrag();
    return true;
  });

  /**
   * A tab released outside its own window: onto another Veld window, or onto
   * nothing.
   *
   * **Commits the target the drag already worked out — it does not recompute
   * one.** The poll has been resolving which window the cursor is over for the
   * whole gesture, and the window under it has been resolving where inside
   * itself; between them the answer is settled well before the release. Every
   * earlier version asked again here instead — from `dragend`'s coordinates
   * (which are the drag's *start*, or `0,0`), then from a bounds test (which
   * cannot see that two windows overlap) — and each was wrong in its own way.
   * Asking at release is the mistake; the drag is the only thing that watched
   * it happen.
   *
   * Onto a window that owns this worktree → the tabs move there, placed where
   * that window was previewing. Anywhere else, including back over the source →
   * a new detached window, which is what drag-out already did.
   */
  /** The receiving window reporting which tabs it actually placed. */
  ipcMain.handle("veld:window:drop-applied", (event, payload) => {
    const record = recordFor(senderWindow(event));
    const pending = record ? pendingDrops.get(payload?.dropId) : undefined;
    // Bound to the window the drop was *sent* to. Ids are sequential from 1, so
    // without this any renderer could settle another window's drop with an
    // invented list — and the source would release tabs nobody placed, which is
    // the vanished-pane outcome this protocol exists to prevent.
    if (!pending || pending.targetId !== record.id) return false;
    pendingDrops.delete(payload.dropId);
    pending.settle(Array.isArray(payload?.accepted) ? payload.accepted.filter(isViewId) : []);
    return true;
  });

  ipcMain.handle("veld:window:drop-out", async (event, payload) => {
    const from = senderWindow(event);
    const fromRecord = recordFor(from);
    if (!from || !fromRecord) return { moved: false, opened: false };
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    const tabs = safeTransferTabs(payload?.tabs);
    if (worktreeId === null || tabs.length === 0) {
      return { moved: false, opened: false, reason: "invalid" };
    }

    // Which window the pointer was over is what the poll has been tracking all
    // along, not a bounds test taken here. The renderer's own "the pointer left
    // me" (`dragleave`, routed by the OS) settles source-versus-target, which
    // geometry cannot — a point inside the window on top is inside the one
    // beneath it too. It does *not* settle target-versus-target; see `pollDrag`.
    endDrag();
    const over = lastOverId === null ? null : allRecords().find((r) => r.id === lastOverId);
    // A window this worktree's panes belong in — `ownsWorktree` in
    // `windowState.js` — plus the two things that are not set arithmetic: it has
    // to still exist, and it must not be the window the drag started in.
    // A window that is closing is not a target at all, so the drop falls through to
    // a new detached window rather than being refused: the tabs stay visible, which
    // beats both losing them and leaving them behind.
    const owns =
      over &&
      !over.win.isDestroyed() &&
      !over.closing &&
      over.id !== fromRecord.id &&
      ownsWorktree(over, worktreeId, showing);
    const target = owns ? over : undefined;

    if (target) {
      // **A claim outlives the `PaneArea` that can answer for it**, so ask first
      // whether there is a listener to push at. There is not while the window is
      // reloading, while its first `/api/repos` is in flight, or while it sits on
      // the waiting page through a daemon restart — and pushing anyway went
      // nowhere, timed out after `DROP_ACK_MS`, and reported `refused`, which the
      // source turns into "The desktop shell refused the request" two seconds
      // after a gesture that looked like it worked.
      // Whether the app is even on screen is the shell's *own* knowledge, and it
      // covers the whole gap before the page exists whatever the UI's version.
      // `isLoading()` alone does **not**: it is false before the first `loadURL`
      // (`loadAppWhenReady` starts no load until `daemonReachable()` answers) and
      // false again while the `data:` waiting page sits there through a daemon
      // restart — the longest gap of the lot, and one of the cases this is for. So
      // ask what is loaded as well as whether it is still loading.
      //
      // This is what makes the case the issue is actually about fast: a detached
      // window is a valid drop target from the instant the main process opens it
      // (its `worktreeId` is set there, with no renderer involved), while the
      // renderer cannot report a listener until `PaneArea` has mounted.
      const showingApp = showsApp(target.win);
      if (
        !showingApp ||
        target.win.webContents.isLoading() ||
        dropDelivery(target.dropListener) === "queue"
      ) {
        return queueDrop(target, worktreeId, tabs);
      }
      // `drop-here`, not the hand-back queue: the target has been previewing a
      // *position* — an edge to split at, or a place in its tab strip — and
      // that is where these belong. The queue exists for a closing window's
      // tabs, which have no pointer behind them and can only be appended.
      const dropId = nextDropId++;
      const accepted = await new Promise((resolve) => {
        pendingDrops.set(dropId, { targetId: target.id, settle: resolve });
        setTimeout(() => {
          if (pendingDrops.delete(dropId)) resolve(null);
        }, DROP_ACK_MS);
        target.win.webContents.send("veld:window:drop-here", { dropId, worktreeId, tabs });
      });
      // Never answered — a renderer that has a listener registered and still did
      // not reply, or a window that closed mid-drop. Same fallback as an
      // unmounted one: the queue, which the main process owns and hands on if the
      // window closes. `null`, not an empty list: a handler that answers "I
      // placed nothing" is a refusal to respect, and re-delivering it through the
      // queue would insert tabs its own validation had just rejected.
      if (accepted === null) return queueDrop(target, worktreeId, tabs);
      if (accepted.length === 0) return { moved: false, opened: false, reason: "refused" };
      if (target.win.isMinimized()) target.win.restore();
      target.win.show();
      target.win.focus();
      return { moved: true, opened: false, accepted };
    }

    if (!canOpenAnother(windows.size)) return { moved: false, opened: false, reason: "cap" };
    const seed = buildSeedLayout(worktreeId, tabs, payload?.ratio);
    if (!seed) return { moved: false, opened: false, reason: "invalid" };
    const win = openWindow({
      kind: "detached",
      origin: fromRecord.suffix,
      originId: fromRecord.id,
      originWindow: from,
      seed,
      repoRoot: safeRepoRoot(payload?.repoRoot),
      worktreeId,
    });
    return {
      moved: false,
      opened: win !== null,
      reason: win ? null : "cap",
      accepted: win ? tabs.map((t) => t.id) : [],
    };
  });

  /**
   * Collect tabs handed to this window by a detached one that closed.
   *
   * A queue rather than a push payload, drained by the renderer at mount and on
   * the `veld:window:adopt` nudge — see `handBack` for why a plain `send` loses
   * them.
   */
  ipcMain.handle("veld:window:take-adopted", (event) => {
    const record = recordFor(senderWindow(event));
    if (!record) return [];
    const pending = record.pendingAdopt;
    record.pendingAdopt = [];
    return pending;
  });

  /**
   * Pull tabs out into a window of their own.
   *
   * The renderer removes them from its own layout only *after* this resolves, so
   * a refused detach (the window cap, an unusable payload) leaves the tabs where
   * they are rather than dropping them on the floor. The brief moment where both
   * layouts name the same terminal is safe by construction: an attach takes the
   * session over, which is the same mechanism that makes a reload work.
   */
  ipcMain.handle("veld:window:detach", (event, payload) => {
    const from = senderWindow(event);
    if (!from) return { opened: false, reason: "no-window" };
    if (!canOpenAnother(windows.size)) return { opened: false, reason: "cap" };
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    const tabs = safeTransferTabs(payload?.tabs);
    if (worktreeId === null || tabs.length === 0) {
      return { opened: false, reason: "invalid" };
    }
    const seed = buildSeedLayout(worktreeId, tabs, payload?.ratio);
    if (!seed) return { opened: false, reason: "invalid" };
    const fromRecord = recordFor(from);
    const win = openWindow({
      kind: "detached",
      origin: fromRecord?.suffix ?? null,
      originId: fromRecord?.id ?? null,
      originWindow: from,
      seed,
      repoRoot: safeRepoRoot(payload?.repoRoot),
      worktreeId,
    });
    // The accepted ids, not just `opened`. `safeTransferTabs` drops per tab (an
    // over-long one, a duplicate id, an unknown kind) and truncates the list, so
    // "a window opened" does not mean "all of them went" — and the renderer
    // releases and closes exactly what it is told went. Anything else is a tab
    // removed from the only layout that named it.
    return {
      opened: win !== null,
      reason: win ? null : "cap",
      accepted: win ? tabs.map((t) => t.id) : [],
    };
  });

  /**
   * What this window would hand back if it closed now.
   *
   * Pushed on every layout change rather than asked for at close time: `close`
   * is not a moment at which a renderer can be relied on to answer, and a
   * round-trip there would race the teardown. The cost is one small IPC per tab
   * move in a detached window.
   */
  ipcMain.handle("veld:window:snapshot", (event, payload) => {
    const record = recordFor(senderWindow(event));
    // Detached only, like `set-title` and `close`. A main window's tabs are its
    // own and `handBack` would never read them, so retaining a snapshot for one
    // is memory held in the privileged process for nothing — reachable from a
    // main window navigated to `?chrome=none`, since that parameter is (rightly)
    // forgeable.
    if (!record || record.kind !== "detached") return false;
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    const tabs = safeTransferTabs(payload?.tabs);
    record.snapshot = worktreeId === null || tabs.length === 0 ? null : { worktreeId, tabs };
    // **This is where the seed is retired**, not at `did-finish-load`. A page
    // that has loaded does not yet know its layout: the renderer cannot report
    // one until `/api/repos` resolves the worktree, and if that first request
    // fails it retries five seconds later. Clearing on load left those seconds
    // with neither a snapshot nor a seed, so closing the window in them handed
    // back nothing — and the tabs had been released by the origin at detach.
    // A snapshot arriving is the actual proof that something else now holds the
    // layout — so only a *non-empty* one retires the seed. Clearing it
    // unconditionally would have made the comment above false on the branch two
    // lines up, which nulls the snapshot for an empty payload: the window would
    // then hold neither. Unreachable from a fresh detach today, and one guard
    // removal from losing shells again.
    if (record.snapshot) record.seed = null;
    return true;
  });

  /**
   * This window's tab strip, in drawn order — the input to `cycle-tab` below.
   *
   * Every window reports, main and detached alike, which is the one thing that
   * makes a *global* order possible: a main window's own tabs are half of it.
   * (Contrast `veld:window:shows` and `veld:window:snapshot`, both of which are
   * deliberately one-kind-only.)
   *
   * Pushed on every layout change rather than asked for when a chord is pressed:
   * a request-response per keystroke would have to reach every window's renderer
   * and wait for all of them, and a renderer that is busy or gone would stall or
   * silently truncate the cycle. Retained state answers instantly and degrades to
   * "that window contributes nothing" instead.
   */
  ipcMain.handle("veld:window:tabs", (event, payload) => {
    const record = recordFor(senderWindow(event));
    if (!record) return false;
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    const ids = safeTabIds(payload?.tabIds);
    // **No `activeId` here, deliberately.** Which tab is active is only ever
    // asked about the window that pressed the key, and that window sends it with
    // the request — so a copy retained here would be state nothing reads, kept
    // fresh by an IPC nothing depends on, and wrong in exactly the window where
    // it mattered (the one whose push had not landed yet).
    record.tabs = worktreeId === null || ids.length === 0 ? null : { worktreeId, ids };
    return true;
  });

  /**
   * Step to the next/previous tab of the worktree the calling window is showing,
   * across **every** window showing that worktree.
   *
   * The order is windows by `record.id` — creation order, and never reused, so a
   * detach-and-close cycle cannot renumber it — then each window's tabs as its
   * strip draws them. Wraps at both ends.
   *
   * **Position is derived, not remembered.** It is `(the calling window, the
   * `activeId` that window sent with this very call)`. Nothing about "where the
   * cycle currently is" is stored anywhere, because it does not need to be: after
   * a press lands on another window, that window has OS focus, so the *next*
   * press is handled by its renderer and reports its own real active tab. #315
   * kept this position in the originating renderer and needed an anchor ref to
   * represent "parked on a window I cannot see the tabs of", which went stale and
   * ping-ponged between one tab and one detached window. There is nothing here to
   * go stale.
   *
   * `activeId` comes **in the request** rather than from `record.tabs`: the push
   * above is a separate IPC, and a chord pressed in the same frame as a tab
   * change would otherwise be answered from the previous position.
   *
   * Returns `{ tabId }` when the answer is a tab in the calling window (it
   * activates it itself — the same path a click takes), `{ focused: true }` when
   * another window was raised and pushed at, and `null` when there is nothing to
   * do.
   */
  ipcMain.handle("veld:window:cycle-tab", (event, payload) => {
    const sender = recordFor(senderWindow(event));
    if (!sender) return null;
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    if (worktreeId === null) return null;
    const delta = payload?.delta === -1 ? -1 : payload?.delta === 1 ? 1 : null;
    if (delta === null) return null;
    // The sender may only cycle a worktree it is actually showing — the same
    // check `shows`/`legacy-claim`/drop routing make, and the one thing that
    // stops a payload naming a worktree set the caller has nothing to do with.
    // Without it, a renderer (or a compromised main frame) picks which windows
    // it raises by naming their worktree and sending an `activeId` that is not
    // in the resulting order, which lands deterministically on its first or
    // last entry. Refusing resolves to `null`, and the caller falls back to its
    // own tabs — the same degradation as an older shell.
    if (!ownsWorktree(sender, worktreeId, showing)) return null;

    // Liveness is filtered here rather than in `cycleOrder`, which is pure —
    // the same division `ownsWorktree` makes.
    const live = allRecords().filter((r) => !r.closing && !r.win.isDestroyed());
    const next = nextInCycle(
      cycleOrder(live, worktreeId),
      sender.id,
      safeTabId(payload?.activeId),
      delta,
    );
    if (!next) return null;
    if (next.recordId === sender.id) return { tabId: next.tabId };

    const target = live.find((r) => r.id === next.recordId);
    // Cannot happen — `next` came out of the order built from `live` — but the
    // alternative to checking is a crash in the privileged process on a race
    // this code does not otherwise reason about.
    if (!target) return null;
    if (target.win.isMinimized()) target.win.restore();
    target.win.show();
    target.win.focus();
    // **The keyboard goes to the page, not to whichever child view had it.**
    // Raising a window restores focus to its last-focused child, and if that was
    // a browser pane's `WebContentsView` the page never gets the keyboard —
    // worse, the view's own `focus` event then fires and moves the layout's
    // focused dock back to that pane, so the tab we are about to activate is
    // active while ⌘W and the next cycle step both read the *other* dock. Same
    // call `browserViews.js` makes before forwarding an accelerator, for the
    // same reason.
    target.win.webContents.focus();
    // `show()`/`focus()` are OS-level and this process cannot reach into another
    // renderer's DOM. Which of that window's tabs is active, and where real focus
    // sits inside it, is state only that renderer can change — so it is told, and
    // it runs the *same* activation a click in it would have run.
    target.win.webContents.send("veld:window:activate-tab", { tabId: next.tabId });
    return { focused: true };
  });

  ipcMain.handle("veld:window:set-title", (event, payload) => {
    const win = senderWindow(event);
    const record = recordFor(win);
    // Main windows are titled by the app — see `openWindow`.
    if (!record || record.kind !== "detached") return false;
    win.setTitle(safeTitle(payload?.title) ?? "Veld");
    return true;
  });

  /**
   * Close the calling window.
   *
   * Two callers: a detached window whose last tab was closed closes itself (a
   * bare dock with no dock in it is an empty box with no way to put anything
   * back in it), and ⌘W in **any** window when there is no tab to close — which
   * is what keeps that chord from silently doing nothing now that it is the tab
   * accelerator rather than Electron's `close` role.
   *
   * The `kind !== "detached"` guard this used to carry is gone with the second
   * caller, and it was never a security boundary: a window closing *itself* is
   * something the user can do from the traffic light, the Window menu and ⌘⇧W,
   * and `senderWindow` still pins it to the caller's own window. Closing runs
   * the same `close` path as all of those, hand-back included.
   */
  ipcMain.handle("veld:window:close", (event) => {
    const record = recordFor(senderWindow(event));
    if (!record || record.win.isDestroyed()) return false;
    record.win.close();
    return true;
  });

  /**
   * Show a native OS notification from a terminal's OSC 9 request.
   *
   * Shown from the **main** process rather than the renderer so it is a real
   * native banner regardless of how the window was opened, and so the click can
   * focus this window even when it is backgrounded. On click the window comes
   * forward and the requesting renderer is told which worktree/pane to focus
   * (the payload is echoed back, so the click handler needs no other state).
   */
  ipcMain.handle("veld:app:notify", (event, payload) => {
    const win = BrowserWindow.fromWebContents(event.sender);
    if (!win || win.isDestroyed() || !Notification.isSupported()) {
      // macOS silently drops a notification it cannot show; say why so a
      // missing banner is diagnosable rather than a mystery.
      console.warn(
        "[veld] cannot show a notification: ",
        Notification.isSupported()
          ? "the window is gone"
          : "this OS/app build has no notification support",
      );
      return false;
    }
    const title = String(payload?.title ?? "");
    const body = String(payload?.body ?? "");
    const n = new Notification({ title, body, silent: true });
    n.on("click", () => {
      if (!win.isDestroyed()) {
        if (win.isMinimized()) win.restore();
        win.show();
        win.focus();
      }
      win.webContents.send("veld:app:notify-click", {
        worktreeId: safeWorktreeId(payload?.worktreeId),
        sessionId: String(payload?.sessionId ?? ""),
      });
    });
    // On macOS the system can refuse to display a banner (no notification
    // permission, or "Do Not Disturb") without throwing — this is the only
    // signal. Report it where the user can find it.
    n.on("failed", () => {
      console.warn(
        "[veld] macOS refused to display a notification — grant this app notification permission in System Settings → Notifications (Veld / Electron), and check Do Not Disturb / Focus.",
      );
    });
    n.show();
    return true;
  });
}

function initWindows(dependencies) {
  deps = dependencies;
  // The size/position a fresh main window starts at. Read once, at init: the
  // only path that needs it is opening a window without an explicit bounds
  // (the macOS reopen, a restore fallback, ⌘N), and each of those writes it
  // back the moment it has a live main window to read from.
  lastMainBounds = readLastMainBounds(readStateRaw());
}

// Exactly what `main.js` calls. `recordFor` and `persistWindows` are internal:
// nothing can load this module without Electron, so an unused export here is
// surface with no consumer and no test behind it.
module.exports = {
  initWindows,
  openWindow,
  focusPrimary,
  openSettings,
  tabCommand,
  restoreWindows,
  registerWindowIpc,
  setQuitting,
  windowCount,
};
