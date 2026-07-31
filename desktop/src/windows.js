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
  handBackTarget,
  nextSuffix,
  parseWindowList,
  restoreBudget,
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
 * @property {string | null} origin  which window a detached one came from, as a
 *   *suffix* — the only form that survives into `windows.json`
 * @property {number | null} originId  the same thing for this run, as a record
 *   id. Suffixes are recycled, so after `w2` closes and a new window takes the
 *   number, the persisted `origin: "w2"` names a window its tabs never came
 *   from; the record id never repeats and is what hand-back actually matches on.
 * @property {number} id  monotonic within this process
 * @property {string | null} seed  the layout this window boots with, read once
 *   over `veld:window:seed` and then dropped
 * @property {{worktreeId: number, tabs: object[]} | null} snapshot
 *   what this window would hand back if it closed now — pushed by the renderer
 *   on every layout change, so a hand-back does not depend on the renderer still
 *   being alive when `close` fires.
 * @property {{worktreeId: number, tabs: object[]}[]} pendingAdopt
 *   tabs handed to this window that its renderer has not collected yet
 */

/** @type {Map<number, WindowRecord>} */
const windows = new Map();

/** Never reused, unlike a suffix. See `WindowRecord.originId`. */
let nextRecordId = 1;

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
      additionalArguments: [`--veld-layout-slot=${slot}`, `--veld-window-kind=${kind}`],
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
    seed,
    snapshot: null,
    pendingAdopt: [],
  };
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
  target.pendingAdopt.push(snapshot);
  target.win.webContents.send("veld:window:adopt");
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
   * a promise and this must not be one. **One-shot**: a reload has
   * `sessionStorage` and must not be re-seeded.
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
    const record = recordFor(win);
    event.returnValue = record?.seed ?? null;
    if (record) record.seed = null;
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
    // Detached only, like `set-title` and `close`. A main window's tabs are its
    // own and `handBack` would never read them, so retaining a snapshot for one
    // is memory held in the privileged process for nothing — reachable from a
    // main window navigated to `?chrome=none`, since that parameter is (rightly)
    // forgeable.
    if (!record || record.kind !== "detached") return false;
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
