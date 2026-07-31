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
  safeRepoRoot,
  safeTitle,
  safeTransferTabs,
  safeWorktreeId,
} = require("./validate");
const {
  canOpenAnother,
  nextSuffix,
  parseWindowList,
  safeBounds,
  serializeWindowList,
  slotFor,
} = require("./windowState");

/**
 * @typedef {object} WindowRecord
 * @property {BrowserWindow} win
 * @property {string | null} suffix  `null` for the first window (bare base slot)
 * @property {string} slot
 * @property {"main" | "detached"} kind
 * @property {string | null} origin  which window a detached one came from
 * @property {{worktreeId: number, tabs: object[]} | null} snapshot
 *   what this window would hand back if it closed now — pushed by the renderer
 *   on every layout change, so a hand-back does not depend on the renderer still
 *   being alive when `close` fires.
 */

/** @type {Map<number, WindowRecord>} */
const windows = new Map();

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
          // Normal bounds, not `getBounds()`: a window remembered while
          // maximised or full-screen restores as a window with no way back to
          // the size it had before.
          bounds: safeBounds(r.win.getNormalBounds()),
        }));
      fs.mkdirSync(path.dirname(deps.stateFile), { recursive: true });
      fs.writeFileSync(
        deps.stateFile,
        serializeWindowList(readStateRaw(), deps.slotBase, records),
      );
    } catch {
      // An unwritable userData costs the window set on the next launch and
      // nothing else. The app still runs.
    }
  }, 400);
}

// ---------------------------------------------------------------------------
// Creating windows
// ---------------------------------------------------------------------------

function appUrl({ kind, repoRoot, worktreeId }) {
  const params = new URLSearchParams({ shell: "electron" });
  // A detached window is one dock and nothing else. Unlike the layout slot this
  // is *fine* in the URL: it hides chrome and grants nothing, so a forged
  // `?chrome=none` in a browser tab is a page with no top bar, not access to
  // anything. The slot stays on the preload bridge for the opposite reason.
  if (kind === "detached") params.set("chrome", "none");
  if (repoRoot) params.set("repo", repoRoot);
  if (worktreeId) params.set("wt", String(worktreeId));
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
      await win.loadURL(url);
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
  const point = screen.getCursorScreenPoint();
  const display = screen.getDisplayNearestPoint(point);
  const area = display.workArea;
  const from = originWin && !originWin.isDestroyed() ? originWin.getNormalBounds() : null;
  const x = from ? from.x + 48 : area.x + Math.round((area.width - size.width) / 2);
  const y = from ? from.y + 48 : area.y + Math.round((area.height - size.height) / 2);
  return {
    width: Math.min(size.width, area.width),
    height: Math.min(size.height, area.height),
    x: Math.max(area.x, Math.min(x, area.x + area.width - Math.min(size.width, area.width))),
    y: Math.max(area.y, Math.min(y, area.y + area.height - Math.min(size.height, area.height))),
  };
}

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
      additionalArguments: [
        `--veld-layout-slot=${slot}`,
        `--veld-window-kind=${kind}`,
        ...(seed
          ? [`--veld-window-seed=${Buffer.from(seed, "utf8").toString("base64")}`]
          : []),
      ],
    },
  });

  /** @type {WindowRecord} */
  const record = { win, suffix, slot, kind, origin, snapshot: null };
  windows.set(win.id, record);

  // Run URLs open in the user's real browser, never inside the shell.
  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: "deny" };
  });

  const url = appUrl({ kind, repoRoot, worktreeId });
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

  if (detached) {
    // The page owns this window's title, and sets it from the active tab.
    win.setTitle("Veld");
  } else {
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
    windows.delete(win.id);
    persistWindows();
  });

  void loadAppWhenReady(win, url).catch(() => {
    // A window closed mid-load. Nothing to report and nothing to retry.
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
  if (record.kind !== "detached") return;
  const snapshot = record.snapshot;
  if (!snapshot || snapshot.tabs.length === 0) return;

  const others = allRecords().filter((r) => r !== record && !r.win.isDestroyed());
  // The window it came from first — including another detached one, since a tab
  // can be pulled out of a detached window in turn and "back" means back there.
  // Otherwise any main window, because the alternative is nowhere.
  const target =
    others.find((r) => r.suffix === record.origin) ??
    others.find((r) => r.kind === "main") ??
    null;
  if (!target) return;
  target.win.webContents.send("veld:window:adopt", snapshot);
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
  const opened = [];
  for (const entry of stored) {
    // The stored list is already capped, but the fallback below can add one
    // more — a file holding `MAX_WINDOWS` detached windows would otherwise
    // reopen as one over the ceiling.
    if (!canOpenAnother(windows.size + 1)) break;
    const win = openWindow({
      kind: entry.kind,
      suffix: entry.suffix,
      origin: entry.origin,
      bounds: entry.bounds,
    });
    if (win) opened.push(win);
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
  ipcMain.handle("veld:window:new", (event) => {
    if (!senderWindow(event)) return { opened: false };
    return { opened: openWindow({ kind: "main" }) !== null };
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
    const win = openWindow({
      kind: "detached",
      origin: recordFor(from)?.suffix ?? null,
      originWindow: from,
      seed,
      repoRoot: safeRepoRoot(payload?.repoRoot),
      worktreeId,
    });
    return { opened: win !== null, reason: win ? null : "cap" };
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
    if (!record) return false;
    const worktreeId = safeWorktreeId(payload?.worktreeId);
    const tabs = safeTransferTabs(payload?.tabs);
    record.snapshot = worktreeId === null || tabs.length === 0 ? null : { worktreeId, tabs };
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

module.exports = {
  initWindows,
  openWindow,
  focusPrimary,
  restoreWindows,
  registerWindowIpc,
  setQuitting,
  windowCount,
  recordFor,
  persistWindows,
};
