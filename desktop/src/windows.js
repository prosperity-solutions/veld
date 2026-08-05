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

const { BrowserWindow, screen, shell } = require("electron");
const fs = require("node:fs");
const path = require("node:path");
const {
  buildSeedLayout,
  isViewId,
  safeRepoRoot,
  safeTitle,
  safeTransferTabs,
  safeWorktreeId,
  transferFromSeed,
} = require("./validate");
const {
  canOpenAnother,
  dropDelivery,
  forgetWorktrees,
  handBackTarget,
  nextSuffix,
  othersHolding,
  ownsWorktree,
  parseWindowList,
  releaseClaims: releaseClaimsIn,
  releaseHolds: releaseHoldsIn,
  restoreBudget,
  safeBounds,
  setHolds: setHoldsIn,
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
 * @property {"unknown" | "ready" | "gone"} dropListener
 *   whether this window has a live listener for a cross-window drop. `unknown`
 *   until the renderer says — which an older `/ide` bundle never does, and which
 *   is why "unknown" is not "gone"; see `dropDelivery` in `windowState.js`.
 * @property {number | null} worktreeId
 * @property {string | null} repoRoot  which worktree a *detached* window is a
 *   dock for. Persisted and put back in its URL on restore: a bare dock has no
 *   rail, so without this it reopened against whatever the main window last
 *   selected and came back blank, with its real tabs unread in its own slot.
 */

/** @type {Map<number, WindowRecord>} */
const windows = new Map();

/** Never reused, unlike a suffix. See `WindowRecord.originId`. */
let nextRecordId = 1;

/**
 * Which window is showing which worktree: `worktreeId → record id`.
 *
 * **A worktree has one set of panes, and one window shows it.** Layouts live in
 * a store shared by every window (`LAYOUT_WORKTREE_KEY` in `panes/model.ts`), so
 * without this two windows would render the same set and both attach to its
 * terminals — and an attach *takes a session over*, so they would trade every
 * shell back and forth. The per-window layout store used to prevent that by
 * giving each window a private copy, which is worse: it prevented the collision
 * by creating the confusion, two independent sets of panes for one worktree with
 * no way to tell which window held which shells.
 *
 * A claim is *displayed*, not owned. A window that switches away releases, and
 * the layout stays in the shared store for whoever picks the worktree up next —
 * which is what makes "close that window and it comes back over here" work with
 * no hand-off protocol at all.
 *
 * Detached windows never claim: their tabs were transferred out of a worktree
 * their origin is showing, and they are a satellite of that claim.
 */
const claims = new Map();

/**
 * Which windows still *hold* a worktree's panes, as opposed to showing them:
 * `worktreeId → Set<record id>`.
 *
 * A window keeps the panes of worktrees it has visited, mounted and attached, so
 * switching back to one is instant rather than a reconnect-and-replay. That is
 * worth keeping, and it is also exactly what would make two windows fight: the
 * moment another window claims a worktree, every other holder has to let go of
 * that one — and only that one.
 *
 * Reported by each main window's renderer whenever its layouts change, because
 * the shell cannot see what a page is holding. A window that never reports (an
 * older build, a renderer that died) simply holds nothing here, which degrades
 * to the previous behaviour rather than to a wrong one.
 */
const holders = new Map();

/**
 * Yields waiting for a holder to confirm it has let go: `yieldId → {targetId,
 * settle}`.
 *
 * **The claimer must not attach until the previous holder has released.** Same
 * rule the drop path follows — the source never lets go until the destination
 * acknowledges — applied to the other direction. A bare `send` and a claim
 * recorded regardless leaves a window in which two renderers both hold the same
 * worktree's layout and both consider themselves the owner of its PTY sessions,
 * and a second attach *takes a session over* rather than mirroring it, so the
 * loser's terminal goes dead-but-visible. Short window, and exactly the shape of
 * thing that shows up on a loaded machine and nowhere else.
 *
 * The timeout resolves rather than rejecting: a holder that cannot answer must
 * not be able to block a claim forever, so the fallback is the behaviour this
 * replaced.
 */
const pendingYields = new Map();
let nextYieldId = 1;
/** Long enough for a renderer to drop a layout and release its terminals — this
 *  is a React commit, not a network round trip — and short enough that a wedged
 *  holder is not felt as a broken click. */
const YIELD_ACK_MS = 1500;

/**
 * Tell every window except `keeper` to let go of `worktreeId`, and resolve once
 * they have (or once the timeout gives up on them).
 *
 * "Let go", not "close": the renderer drops that worktree's layout and releases
 * its terminals without ending them, so the shells keep running and the layout
 * stays in the shared store for the window that just claimed it. Sent only for
 * the one worktree being claimed, so a window's other panes are untouched.
 */
function yieldWorktree(worktreeId, keeper) {
  const ids = othersHolding(holders, worktreeId, keeper);
  const targets = allRecords().filter((r) => !r.win.isDestroyed() && ids.includes(r.id));
  return Promise.all(
    targets.map(
      (record) =>
        new Promise((resolve) => {
          const yieldId = nextYieldId++;
          pendingYields.set(yieldId, { targetId: record.id, settle: resolve });
          setTimeout(() => {
            if (pendingYields.delete(yieldId)) resolve();
          }, YIELD_ACK_MS);
          record.win.webContents.send("veld:window:yield", { worktreeId, yieldId });
        }),
    ),
  );
}

/**
 * Stop waiting on a window that is gone.
 *
 * Without this a claim waits out the full timeout for a holder that closed
 * mid-yield — the answer is already known, and making the user feel it as a
 * stalled click is the failure mode the timeout exists to bound, not one to
 * accept.
 */
function settleYieldsFor(recordId) {
  for (const [yieldId, pending] of [...pendingYields]) {
    if (pending.targetId !== recordId) continue;
    pendingYields.delete(yieldId);
    pending.settle();
  }
}

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

/** How many un-drained transfers may be parked on one window. A drop is only
 *  queued for a window that is loading, so a queue this deep means the renderer
 *  is not coming back — and the tabs are better off staying where they are than
 *  piling up somewhere nothing reads. */
const MAX_PENDING_ADOPT = 8;

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

/** The live record showing `worktreeId`, or `null`. */
function claimHolder(worktreeId) {
  const id = claims.get(worktreeId);
  if (id === undefined) return null;
  const holder = allRecords().find((r) => r.id === id && !r.win.isDestroyed());
  if (!holder) {
    claims.delete(worktreeId);
    return null;
  }
  return holder;
}

/**
 * Tell every main window which worktrees *other* windows are showing.
 *
 * Personalised per window on purpose: what a rail needs to grey out is "not
 * mine", and a window computing that from a shared map would first have to
 * learn its own record id — one more thing to keep in sync for no gain.
 *
 * Pushed rather than polled, and re-sent on every claim, every window close and
 * every load, because the interesting transitions all happen in another window:
 * a rail that only refreshed when *you* did something would stay wrong for as
 * long as you did nothing.
 */
function pruneClaims() {
  const ids = new Set(allRecords().filter((r) => !r.win.isDestroyed()).map((r) => r.id));
  let dropped = false;
  // `claimHolder` prunes lazily, one worktree at a time, and it is not on every
  // path — a dead window's claim left here would grey a rail row out with no
  // window behind it and no way for the user to reach it again.
  for (const [worktreeId, id] of claims) {
    if (ids.has(id)) continue;
    claims.delete(worktreeId);
    dropped = true;
  }
  return dropped;
}

function broadcastClaims() {
  pruneClaims();
  const live = allRecords().filter((r) => !r.win.isDestroyed());
  for (const record of live) {
    if (record.kind !== "main") continue;
    const worktreeIds = [];
    for (const [worktreeId, id] of claims) {
      if (id !== record.id) worktreeIds.push(worktreeId);
    }
    record.win.webContents.send("veld:window:claims", { worktreeIds });
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
    try {
      const records = allRecords()
        .filter((r) => !r.win.isDestroyed())
        .map((r) => ({
          suffix: r.suffix,
          kind: r.kind,
          origin: r.origin,
          worktreeId: r.worktreeId,
          repoRoot: r.repoRoot,
          // Normal bounds, not `getBounds()`: a window remembered while
          // maximised or full-screen restores as a window with no way back to
          // the size it had before.
          bounds: safeBounds(r.win.getNormalBounds()),
        }));
      fs.mkdirSync(path.dirname(deps.stateFile), { recursive: true });
      // Write-then-rename, because a torn file is worse than a stale one: a
      // truncated `windows.json` parses to `[]`, the next launch opens a single
      // window, and the first persist after that makes the loss of every other
      // window's layout — and the live shells its terminal ids name —
      // permanent.
      const tmp = `${deps.stateFile}.tmp`;
      fs.writeFileSync(tmp, serializeWindowList(readStateRaw(), deps.slotBase, records));
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
  // opens a dialog the user could have opened from the gear.
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
    (detached ? detachBounds(options.originWindow ?? null) : { width: 1280, height: 800 });

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
          trafficLightPosition: {
            x: 13,
            y: Math.round((deps.topbarHeight - deps.trafficLightSize) / 2),
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
    pendingAdopt: [],
    dropListener: "unknown",
  };
  windows.set(win.id, record);

  // A page navigating away takes its listeners with it, and the renderer gets no
  // chance to say so — an unload handler is not a place to await an IPC round
  // trip. So the shell notices instead: a reload is the longest window in which a
  // claim is routable with nothing behind it (this fires before the new document
  // commits, which errs towards queueing and is the safe direction).
  //
  // `ready → gone`, and `unknown` left alone: a UI that never reports must not be
  // demoted to "gone" by its first reload, or every later cross-window drop into
  // it would be queued instead of placed where the pointer was.
  //
  // Main frame and a real document swap, not `did-start-loading`: that one is the
  // tab spinner, which an iframe's load also turns, and demoting on one would
  // route every later drop to the queue with nothing to undo it.
  win.webContents.on("did-start-navigation", (details) => {
    if (!details.isMainFrame || details.isSameDocument) return;
    if (record.dropListener === "ready") record.dropListener = "gone";
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

  win.on("close", () => {
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
    // release, and it is not going to place a drop. Both fall back on their own,
    // but only after a timeout the user would feel as a stalled gesture.
    settleYieldsFor(record.id);
    settleDropsFor(record.id);
    releaseClaimsIn(claims, record.id);
    releaseHoldsIn(holders, record.id);
    windows.delete(win.id);
    persistWindows();
    // Its worktrees are pickable again, and the rails saying otherwise are in
    // other windows — nothing else would correct them.
    broadcastClaims();
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
  const transfers = own && own.tabs.length > 0 ? [...carried, own] : carried;
  if (transfers.length === 0) return;

  // The precedence — record id, then persisted suffix, then any main window —
  // is `handBackTarget` in `windowState.js`, where it is a decision over plain
  // records and therefore has tests.
  const others = allRecords().filter((r) => r !== record && !r.win.isDestroyed());
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
  if (target.win.isDestroyed()) return { moved: false, opened: false, reason: "refused" };
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
   * Ask to show a worktree.
   *
   * `{ok: true}` and the window renders it — including when this window already
   * held it, so a re-select is not a fight with itself. `{ok: false}` and
   * another window is already showing it, which is not an error to report but a
   * place to go: the shell **focuses that window**, and the caller stays where
   * it is. That is the whole answer to "two sets of panes for one worktree" —
   * there is only ever one, and picking it twice takes you to it.
   *
   * A detached window never claims; it is a satellite of its origin's claim.
   *
   * **It resolves only once every other holder has let go**, because the caller
   * renders the worktree's panes — and attaches to the PTY sessions its layout
   * names — on the strength of this answer. The claim itself is recorded first
   * and synchronously: it is what makes a third window's claim arriving during
   * the wait get refused rather than granted alongside this one, and the greyed
   * rail row in every other window is true from that moment on.
   */
  ipcMain.handle("veld:window:claim", async (event, payload) => {
    const record = recordFor(senderWindow(event));
    if (!record) return { ok: false, reason: "no-window" };
    if (record.kind !== "main") return { ok: true };
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    if (worktreeId === null) return { ok: false, reason: "invalid" };

    const holder = claimHolder(worktreeId);
    if (holder && holder.id !== record.id) {
      // Only a *deliberate* pick raises the other window. A window working out
      // what it may display asks about several worktrees in a row, and having
      // each refusal yank a different window forward would be a window manager
      // fighting the user over a question they did not ask.
      if (payload?.focusHolder !== false) {
        if (holder.win.isMinimized()) holder.win.restore();
        holder.win.show();
        holder.win.focus();
      }
      return { ok: false, reason: "shown-elsewhere" };
    }
    // One *displayed* worktree per window. Releasing the old claim does not
    // make this window let go of that worktree's panes — it keeps them mounted
    // so switching back is instant, and only lets go when somebody else claims
    // that one.
    releaseClaimsIn(claims, record.id);
    claims.set(worktreeId, record.id);
    // Both halves changed: the worktree this window let go of is free for
    // everyone, and the one it took is now spoken for.
    broadcastClaims();
    // Which is now. Any other window still holding this worktree's panes has to
    // let go before this one attaches, or the two would trade its shells — so
    // this is awaited, and the caller learns it may proceed only afterwards.
    // Nothing is re-read after the await, so a claimer that closed during it
    // needs no guard here: `closed` has already dropped its claim, and this
    // answer goes to a renderer that is gone.
    await yieldWorktree(worktreeId, record.id);
    return { ok: true };
  });

  /**
   * What this window currently holds the panes of.
   *
   * The shell cannot see inside a page, and "who would I have to ask to let go"
   * is a question only the pages can answer. Pushed on every layouts change,
   * which is cheap — a list of numbers.
   */
  ipcMain.handle("veld:window:holds", (event, payload) => {
    const record = recordFor(senderWindow(event));
    if (!record || record.kind !== "main") return false;
    // Bounded like every other payload crossing this boundary — a renderer
    // reporting an unbounded list would grow `holders` without limit.
    const ids = Array.isArray(payload?.worktreeIds)
      ? payload.worktreeIds.slice(0, MAX_HELD_WORKTREES).map(safeWorktreeId).filter((id) => id !== null)
      : [];
    setHoldsIn(holders, record.id, ids);
    return true;
  });

  /**
   * A holder confirming it has let go of a worktree.
   *
   * Sent *after* the release has actually happened — the renderer drops the
   * layout and releases its terminals in one commit and acknowledges once that
   * commit is on screen, not when the message arrived. That is the whole content
   * of the ack: before it, the claiming window must not attach.
   *
   * Bound to the window the yield was sent to, like `drop-applied` beside it: ids
   * are sequential from 1, so without the check any renderer could answer for a
   * holder that has not released and hand the claimer a false all-clear.
   */
  ipcMain.handle("veld:window:yielded", (event, payload) => {
    const record = recordFor(senderWindow(event));
    const pending = record ? pendingYields.get(payload?.yieldId) : undefined;
    if (!pending || pending.targetId !== record.id) return false;
    pendingYields.delete(payload.yieldId);
    pending.settle();
    return true;
  });

  /**
   * Whether this window has a listener for a cross-window drop.
   *
   * Reported by the renderer around the lifetime of the handler itself, because
   * that is the only thing that knows: a claim says which worktree a window is
   * showing, not whether the page holding it has mounted. See `dropDelivery` in
   * `windowState.js` for why a window that has never reported is *sent* to rather
   * than queued for.
   */
  ipcMain.handle("veld:window:drops-ready", (event, payload) => {
    const record = recordFor(senderWindow(event));
    if (!record) return false;
    record.dropListener = payload?.ready === true ? "ready" : "gone";
    return true;
  });

  /**
   * Worktrees that have just been deleted, so nothing keeps claiming them.
   *
   * Sent by whichever window did the deleting, because it is the only place the
   * exact ids are known. Not restricted to what the sender itself holds — the
   * claim being dropped usually belongs to *another* window, which is the window
   * that was showing the worktree that has now been removed.
   *
   * See `forgetWorktrees` for why a leftover entry matters: worktree rowids are
   * reused, so the ghost lands on a *new* worktree and reports it as open
   * somewhere it is not.
   */
  ipcMain.handle("veld:window:worktrees-gone", (event, payload) => {
    const record = recordFor(senderWindow(event));
    // Main windows only, like `holds` beside it. A detached window is a satellite
    // with no rail and no delete surface, so it has no business editing global
    // ownership — and this handler is the one that accepts ids the sender does not
    // hold, which is exactly why it should accept them from fewer callers.
    if (!record || record.kind !== "main") return false;
    const ids = Array.isArray(payload?.worktreeIds)
      ? payload.worktreeIds
          .slice(0, MAX_HELD_WORKTREES)
          .map(safeWorktreeId)
          .filter((id) => id !== null)
      : [];
    if (ids.length === 0) return false;
    forgetWorktrees(claims, holders, ids);
    // Every rail is showing the old answer until it is told otherwise.
    broadcastClaims();
    return true;
  });

  /**
   * Which worktrees another window is showing.
   *
   * The push in `broadcastClaims` covers every change from here on; this covers
   * the state a page boots into, which no push can — the window may well have
   * been opened after the claims it needs to know about were made.
   */
  ipcMain.handle("veld:window:claims", (event) => {
    const record = recordFor(senderWindow(event));
    if (!record) return [];
    // Every other mutator of `claims` broadcasts, and a read that prunes is a
    // mutator: dropping an entry here and telling nobody would leave the *other*
    // rails greying out a worktree whose window is gone.
    const dropped = pruneClaims();
    const worktreeIds = [];
    for (const [worktreeId, id] of claims) {
      if (id !== record.id) worktreeIds.push(worktreeId);
    }
    if (dropped) broadcastClaims();
    return worktreeIds;
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
    const owns =
      over && !over.win.isDestroyed() && over.id !== fromRecord.id && ownsWorktree(over, worktreeId, claims);
    const target = owns ? over : undefined;

    if (target) {
      // **A claim outlives the `PaneArea` that can answer for it**, so ask first
      // whether there is a listener to push at. There is not while the window is
      // reloading, while its first `/api/repos` is in flight, or while it sits on
      // the waiting page through a daemon restart — and pushing anyway went
      // nowhere, timed out after `DROP_ACK_MS`, and reported `refused`, which the
      // source turns into "The desktop shell refused the request" two seconds
      // after a gesture that looked like it worked.
      if (dropDelivery(target.dropListener) === "queue") return queueDrop(target, worktreeId, tabs);
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

  ipcMain.handle("veld:window:set-title", (event, payload) => {
    const win = senderWindow(event);
    const record = recordFor(win);
    // Main windows are titled by the app — see `openWindow`.
    if (!record || record.kind !== "detached") return false;
    win.setTitle(safeTitle(payload?.title) ?? "Veld");
    return true;
  });

  /** A detached window whose last tab was closed closes itself: a bare dock with
   *  no dock in it is an empty box with no way to put anything back in it. */
  ipcMain.handle("veld:window:close", (event) => {
    const record = recordFor(senderWindow(event));
    if (!record || record.kind !== "detached") return false;
    record.win.close();
    return true;
  });
}

function initWindows(dependencies) {
  deps = dependencies;
}

// Exactly what `main.js` calls. `recordFor` and `persistWindows` are internal:
// nothing can load this module without Electron, so an unused export here is
// surface with no consumer and no test behind it.
module.exports = {
  initWindows,
  openWindow,
  focusPrimary,
  openSettings,
  restoreWindows,
  registerWindowIpc,
  setQuitting,
  windowCount,
};
