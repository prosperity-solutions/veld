// Veld Desktop — Electron shell around the veld daemon's /ide UI.
//
// Deliberately thin (see desktop/ARCHITECTURE.md): a frameless window that
// loads the daemon-served UI, a macOS tray with run status, embedded browser
// panes (src/browserViews.js), and nothing else. The web UI must stay fully
// usable in a plain browser — the browser panes have an iframe fallback there —
// so everything the shell adds is presentation (native title bar), ambient
// status (tray), or a capability a page genuinely cannot have.

const {
  app,
  BrowserWindow,
  Menu,
  Tray,
  dialog,
  ipcMain,
  nativeImage,
} = require("electron");
const fs = require("node:fs");
const path = require("node:path");
const { registerBrowserViewIpc, disposeWindow } = require("./browserViews");
const {
  focusPrimary,
  initWindows,
  openWindow,
  registerWindowIpc,
  restoreWindows,
  setQuitting,
  windowCount,
  openSettings,
} = require("./windows");
const { MAX_WINDOWS, canOpenAnother } = require("./windowState");
const {
  checkForUpdates,
  initUpdater,
  noteDaemonVersion,
  skewMenuItem,
} = require("./updater");

/**
 * Brand assets, generated from the repo's canonical sources by
 * `scripts/make-icons.py` (see that file for what comes from where).
 *
 * `icon.png` is the app icon — the same rounded-tile mark the favicon shows in a
 * browser tab. `trayTemplate.png` is the menu-bar icon and carries the
 * `logo.svg` mark; the `Template` suffix is what makes macOS tint it for the
 * current menu bar, which is the only way one asset stays legible in light
 * *and* dark mode.
 */
const ASSETS = path.join(__dirname, "..", "assets");
const APP_ICON = path.join(ASSETS, "icon.png");
const TRAY_ICON = path.join(ASSETS, "trayTemplate.png");

// Dev override: point the shell at the vite dev server
// (VELD_DESKTOP_URL=http://localhost:5199). Default: the daemon directly —
// no Caddy/helper needed.
// The app's own name, which macOS uses for the application menu and the About
// item. A packaged build gets this from the bundle; an unpackaged `npm start`
// would otherwise call itself "Electron".
app.setName("Veld");

// The page URL itself is built per window in `windows.js` — a detached window
// carries `chrome=none` and the selection it was pulled out of.
const BASE_URL = process.env.VELD_DESKTOP_URL ?? "http://127.0.0.1:19899";
const HEALTH_URL = `${BASE_URL}/api/health`;
const ENVIRONMENTS_URL = `${BASE_URL}/api/environments`;
const REPOS_URL = `${BASE_URL}/api/repos`;

/**
 * Height of the UI's top bar in the Electron build (`.topbar.electron` in
 * `crates/veld-daemon/ui/src/styles.css`) and the diameter of a macOS traffic
 * light. Together they place the buttons, which the OS draws — no CSS can move
 * them, so without this they sit at `hiddenInset`'s default and read as
 * misaligned against the bar's own controls. Keep in sync with that stylesheet.
 */
const TOPBAR_HEIGHT = 42;
const TRAFFIC_LIGHT_SIZE = 12;

/** @type {Tray | null} */
let tray = null;
/** Set once the tray exists; the updater calls it when the skew notice changes. */
/** @type {(() => Promise<void>) | null} */
let refreshTray = null;

/**
 * A second launch of an installed app focuses the running one instead of opening
 * a window that would fight it over the same daemon, tray and browser
 * partitions. Only when packaged: two *dev* instances are a normal thing to want
 * (`just dev-desktop` against vite beside `just desktop` against the installed
 * daemon), and they share one lock because they share one appId.
 */
const isPrimaryInstance = !app.isPackaged || app.requestSingleInstanceLock();
if (!isPrimaryInstance) app.quit();

/**
 * The **process's** half of a layout slot; `windows.js` adds the per-window half.
 *
 * A layout names live PTY session ids, and a second attach to a session *takes
 * it over* rather than mirroring it — so two renderers restoring one layout
 * would trade every shell back and forth indefinitely. A slot is what keeps them
 * apart, and it must be stable for one window across restarts and different
 * between any two live windows. Two independent things can collide, so the slot
 * has two parts: this base separates *processes*, and the suffix
 * (`slotFor`/`nextSuffix` in `windowState.js`) separates windows within one.
 *
 * Derived from `isPackaged`, deliberately **not** from the single-instance lock.
 * Asking for the lock unpackaged looked like a free way to tell a first instance
 * from a second, and it was not: the lock is per appId and first-caller-wins, so a
 * dev instance holding it made the *packaged* app quit on launch, and whichever
 * happened to start first got the durable slot — a dev run inheriting the
 * installed app's terminals, the exact opposite of the intent.
 *
 * Two concurrent dev instances (a normal thing to want, per the comment above) do
 * share `dev`, and would fight over a shell if both restored the same layout.
 * `claimSlot` is what prevents that: the second one finds the first's live pid in
 * the slot's lockfile and takes a base of its own.
 */
const SLOT_BASE = claimSlot(app.isPackaged ? "main" : "dev");

/**
 * Claim `preferred`, or fall back to a slot of our own if a live process holds it.
 *
 * A lockfile with a pid, rather than a timestamp or a heartbeat: liveness is a
 * question the OS can answer exactly (`kill(pid, 0)`), while a timestamp cannot
 * tell "quit five seconds ago" from "still running" — and the five-seconds-ago
 * case is a user relaunching the app, which is precisely when the layout must be
 * restored rather than abandoned.
 */
function claimSlot(preferred) {
  const fs = require("node:fs");
  const path = require("node:path");
  // An instance that is about to quit must not touch the lock: it would overwrite
  // the live primary's pid with its own and then exit, leaving a stale lock that
  // sends the *next* launch to a fresh slot — and to no restored terminals.
  if (!isPrimaryInstance) return preferred;
  try {
    const dir = path.join(app.getPath("userData"), "layout-slots");
    fs.mkdirSync(dir, { recursive: true });
    const lock = path.join(dir, `${preferred}.lock`);
    // Scoped to the read alone. Wrapping the write in the same `try` meant the
    // very first launch — when the file does not exist yet — threw ENOENT on this
    // line and skipped straight past the write, so the lock was never created on
    // any launch and this function always returned `preferred`: a no-op that
    // looked like protection.
    let held = Number.NaN;
    try {
      held = Number.parseInt(fs.readFileSync(lock, "utf8"), 10);
    } catch {
      // No lock yet, or unreadable: treat the slot as free and claim it below.
    }
    if (Number.isInteger(held) && held !== process.pid) {
      try {
        // Signal 0 is the existence/permission check only.
        process.kill(held, 0);
        // Somebody is using this slot; take one nobody can be using.
        return `${preferred}-${process.pid}`;
      } catch {
        // Stale lock: the process that wrote it is gone, so the slot is ours —
        // and so are the shells its layout names, which is the whole point.
      }
    }
    fs.writeFileSync(lock, String(process.pid));
  } catch {
    // No userData, an unwritable directory: fall through to the preferred slot.
    // Worst case is two windows sharing one, which is where this started.
  }
  return preferred;
}

// Shown while the daemon is unreachable; self-contained and branded
// (dark tokens + wordmark dot styling from the design handoff).
//
// The commands are spelled out rather than linked, because this screen is what a
// packaged download shows on a machine that has never had veld: the app is a
// shell around a daemon it does not ship, and "install veld" with no command is
// a dead end. Both steps are named because the installer deliberately does not
// run setup (`install.sh` → "no auto-run of veld setup"), and setup is what
// installs the daemon agent this screen is waiting for
// (`commands/setup/unprivileged.rs:38`) — `veld doctor` only *diagnoses* it.
// `-webkit-user-select` is re-enabled on the commands alone — the rest of the
// page is a drag region, which otherwise swallows the selection.
const INSTALL_COMMAND = "curl -fsSL https://veld.oss.life.li/get | bash";
const SETUP_COMMAND = "veld setup unprivileged";
const WAITING_HTML = `<!doctype html><html><head><meta charset="utf-8"><title>Veld</title><style>
  body{margin:0;height:100vh;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:14px;
       background:#0d0e10;color:#98a0a9;font:13px/1.6 system-ui,sans-serif;-webkit-app-region:drag}
  .wm{font-weight:700;font-size:22px;color:#e7e9ec}.wm i{color:oklch(0.74 0.14 158);font-style:normal}
  code{font-family:ui-monospace,monospace;background:#1a1d21;border:1px solid #2a2e35;border-radius:6px;padding:2px 7px}
  code.cmd{-webkit-user-select:text;user-select:text;-webkit-app-region:no-drag;color:#e7e9ec}
  p{max-width:420px;text-align:center;margin:0}
</style></head><body>
  <div class="wm">veld<i>.</i></div>
  <p>Waiting for the veld daemon…</p>
  <p>On a fresh machine, install veld and set it up — no sudo needed:</p>
  <p><code class="cmd">${INSTALL_COMMAND}</code></p>
  <p><code class="cmd">${SETUP_COMMAND}</code></p>
  <p>Already set up? Run <code>veld doctor</code> to see what's wrong. Retrying automatically.</p>
</body></html>`;

/**
 * Liveness plus the daemon's version, which is half of the skew check in
 * `updater.js` — asked here because this is the one request the shell already
 * makes on a schedule.
 *
 * @returns {Promise<boolean>}
 */
async function daemonReachable() {
  try {
    const res = await fetch(HEALTH_URL, { signal: AbortSignal.timeout(2000) });
    if (!res.ok) return false;
    // Reachability is the status line's answer, not the body's: a daemon that
    // answers 200 and is slow to flush would otherwise trip the same 2s signal
    // and bounce a healthy app back to the waiting screen. An unreadable body
    // still calls `noteDaemonVersion(undefined)`, which *clears* a stale skew
    // notice rather than leaving one on screen — the same reason a daemon old
    // enough to have no version field is treated as "nothing to say" rather
    // than skipped.
    const body = await res.json().catch(() => null);
    noteDaemonVersion(body?.version);
    return true;
  } catch {
    return false;
  }
}

// Window creation, the per-window layout slots and the detach/hand-back
// ownership rules live in `windows.js`; this file wires it to the pieces that
// are the app's rather than a window's (the waiting page, the daemon poll, the
// icon, the top bar's geometry).
initWindows({
  baseUrl: BASE_URL,
  waitingHtml: WAITING_HTML,
  daemonReachable,
  appIcon: APP_ICON,
  topbarHeight: TOPBAR_HEIGHT,
  trafficLightSize: TRAFFIC_LIGHT_SIZE,
  slotBase: SLOT_BASE,
  stateFile: path.join(app.getPath("userData"), "windows.json"),
  disposeWindow,
});

/**
 * The menu-bar icon: the veld mark as a macOS template image.
 *
 * `nativeImage.createFromPath` picks up the `@2x` file beside it, and the
 * `Template` filename suffix marks it template — so the mark is tinted for the
 * current menu bar instead of being a white glyph that vanishes in light mode.
 * The accent dot survives as a shape rather than as a colour, which is the
 * trade macOS asks of every menu-bar icon.
 *
 * Falls back to the hand-plotted bitmap below if the asset is missing, because a
 * packaging slip should cost the icon, not the tray.
 */
function trayIcon() {
  const asset = nativeImage.createFromPath(TRAY_ICON);
  if (!asset.isEmpty()) return asset;
  return fallbackTrayIcon();
}

// 16×16 template icon plotted by hand — the fallback, kept because it needs no
// asset at all: a 1-bit "v" in a 16x16 grid.
function fallbackTrayIcon() {
  const size = 16;
  const buf = Buffer.alloc(size * size * 4, 0);
  const set = (x, y) => {
    const i = (y * size + x) * 4;
    buf[i] = 0;
    buf[i + 1] = 0;
    buf[i + 2] = 0;
    buf[i + 3] = 255;
  };
  for (let s = 0; s < 7; s++) {
    // left stroke of the v (2px wide)
    set(3 + s, 4 + s);
    set(4 + s, 4 + s);
    // right stroke
    set(12 - s, 4 + s);
    set(11 - s, 4 + s);
  }
  const img = nativeImage.createFromBitmap(buf, { width: size, height: size });
  img.setTemplateImage(true);
  return img;
}

/** @type {{at: number, marks: Map<string, {alias: string, emoji: string}>}} */
let marksCache = { at: 0, marks: new Map() };
const MARKS_TTL_MS = 60_000;

/**
 * Map every known worktree's checkout path to its emoji + alias, for labelling
 * runs by checkout rather than by project name. Empty map on any failure — the
 * tray must still render when only /api/environments answers.
 *
 * Cached for a minute rather than fetched on the tray's 10s tick: `/api/repos`
 * stats and parses a veld.json per worktree, and aliases change on the order of
 * minutes. A newly imported checkout gets its emoji within one TTL.
 *
 * @returns {Promise<Map<string, {alias: string, emoji: string}>>}
 */
async function worktreeMarks() {
  if (Date.now() - marksCache.at < MARKS_TTL_MS) return marksCache.marks;
  const marks = new Map();
  // Stamped even on failure, keeping the previous marks: against an older daemon
  // with no /api/repos — or none running — retrying every tick would reinstate
  // exactly the per-tick cost this cache exists to avoid.
  try {
    const res = await fetch(REPOS_URL, { signal: AbortSignal.timeout(2000) });
    if (res.ok) {
      const data = await res.json();
      for (const repo of data.repos ?? []) {
        for (const wt of repo.worktrees ?? []) {
          if (wt.path) marks.set(wt.path, { alias: wt.alias, emoji: wt.emoji });
        }
      }
      marksCache = { at: Date.now(), marks };
      return marksCache.marks;
    }
  } catch {
    // Older daemon, or none — keep whatever we had and fall back to the path.
  }
  marksCache = { at: Date.now(), marks: marksCache.marks };
  return marksCache.marks;
}

/**
 * Look up a checkout's mark. Worktree paths are canonicalized daemon-side (git
 * reports realpaths) while a project root is whatever directory veld ran in, so
 * a symlinked checkout only matches after resolving.
 */
function markFor(marks, root) {
  if (!root) return undefined;
  const direct = marks.get(root);
  if (direct) return direct;
  try {
    return marks.get(fs.realpathSync(root));
  } catch {
    return undefined;
  }
}

/** Last two path segments, enough to tell two clones apart in a menu row. */
function shortenPath(p) {
  if (!p) return "unknown path";
  const parts = p.split("/").filter(Boolean);
  return parts.slice(-2).join("/") || p;
}

async function trayMenu() {
  /** @type {Electron.MenuItemConstructorOptions[]} */
  const items = [];
  try {
    const res = await fetch(ENVIRONMENTS_URL, {
      signal: AbortSignal.timeout(2000),
    });
    const data = await res.json();
    const running = [];
    for (const project of data.projects ?? []) {
      for (const run of project.runs ?? []) {
        if (run.status === "running" || run.status === "starting") {
          running.push({
            project: project.name,
            root: project.project_root,
            run,
          });
        }
      }
    }
    items.push({
      label: running.length
        ? `${running.length} running run${running.length > 1 ? "s" : ""}`
        : "No running runs",
      enabled: false,
    });
    const shownRuns = running.slice(0, 10);
    // Two clones of one repo share `project.name` (it comes from veld.json), so
    // their rows would be indistinguishable (#172). Mark those rows with the
    // worktree **emoji** — always the glyph, never the colour, regardless of the
    // `worktree.markerStyle` setting: this label is a plain string handed to the
    // OS, and a CSS custom property means nothing there. The same rule applies to
    // the window title. The glyph is empty only in the window between a worktree
    // being registered and its first sync backfilling one, which is why the label
    // below still guards it. Only the ambiguous rows are marked, since for the
    // single-checkout majority it is noise, and then `/api/repos` isn't fetched at all.
    const nameCounts = new Map();
    for (const { project } of shownRuns) {
      nameCounts.set(project, (nameCounts.get(project) ?? 0) + 1);
    }
    const ambiguous = [...nameCounts.values()].some((n) => n > 1);
    const marks = ambiguous ? await worktreeMarks() : new Map();
    for (const { project, root, run } of shownRuns) {
      let label = `${project} / ${run.name} — ${run.status}`;
      if ((nameCounts.get(project) ?? 0) > 1) {
        const mark = markFor(marks, root);
        const where = mark
          ? `${mark.emoji ? `${mark.emoji} ` : ""}${mark.alias}`
          : shortenPath(root);
        label = `${project} (${where}) / ${run.name} — ${run.status}`;
      }
      items.push({
        label,
        toolTip: root ?? undefined,
        click: () => focusPrimary(),
      });
    }
  } catch {
    items.push({ label: "veld daemon unreachable", enabled: false });
  }
  items.push(
    { type: "separator" },
    { label: "Open Veld Desktop", click: () => focusPrimary() },
    {
      label: "New Window",
      // Disabled rather than hidden at the cap: a row that vanishes reads as a
      // broken menu, while a greyed one says the app is at its limit.
      enabled: canOpenAnother(windowCount()),
      click: () => newWindowOrSayWhyNot(),
    },
    { label: `Version ${app.getVersion()}`, enabled: false },
  );
  const skew = skewMenuItem();
  if (skew) items.push(skew);
  items.push(
    {
      label: "Check for Updates…",
      click: () => void checkForUpdates({ manual: true }),
    },
    { label: "Quit", role: "quit" },
  );
  return Menu.buildFromTemplate(items);
}

/**
 * Open a window, or say why not.
 *
 * The tray's row can grey itself out because the tray is rebuilt every ten
 * seconds; the application menu is built once, and `⌘N` is an accelerator that
 * fires whatever the menu currently claims. So the cap has to be reported at the
 * moment it is hit, or the app's most direct affordance is a key that silently
 * does nothing — which is the same argument the tray row's `enabled` was written
 * for, applied to the surface that actually needs it.
 */
function newWindowOrSayWhyNot() {
  if (openWindow({ kind: "main" })) return;
  void dialog.showMessageBox({
    type: "info",
    message: `Veld Desktop is limited to ${MAX_WINDOWS} windows.`,
    detail: "Close one and try again.",
    buttons: ["OK"],
  });
}

function createTray() {
  tray = new Tray(trayIcon());
  tray.setToolTip("Veld");
  refreshTray = async () => tray?.setContextMenu(await trayMenu());
  void refreshTray();
  setInterval(() => void refreshTray?.(), 10_000);
}

/**
 * The application menu.
 *
 * Electron builds a default one, but it is Electron's — there is nowhere in it
 * to check for updates, and on Linux there is no tray to put that anywhere else.
 * The standard roles are kept verbatim (an app with no Edit menu has no ⌘C), so
 * this template is the default plus a Veld section.
 *
 * Rebuilt whenever the version skew changes, since the skew row lives in it: the
 * tray is macOS-only, and Linux is the platform whose app *can* update itself
 * and therefore the one most likely to end up ahead of its daemon.
 */
function buildAppMenu() {
  const isMac = process.platform === "darwin";
  const veldItems = [
    { label: `Veld Desktop ${app.getVersion()}`, enabled: false },
  ];
  const skew = skewMenuItem();
  if (skew) veldItems.push(skew);
  veldItems.push({
    label: "Check for Updates…",
    click: () => void checkForUpdates({ manual: true }),
  });
  /** @type {Electron.MenuItemConstructorOptions[]} */
  const template = [
    ...(isMac
      ? [
          {
            label: app.name,
            submenu: [
              { role: "about" },
              { type: "separator" },
              // A main-process accelerator, for the same reason ⌘N is one: a
              // focused `WebContentsView` swallows every keystroke, so the page's
              // own ⌘, handler never sees it while a browser pane has focus. The
              // menu is handled before web contents get the key.
              {
                label: "Settings…",
                accelerator: "CmdOrCtrl+,",
                click: () => openSettings(),
              },
              { type: "separator" },
              ...veldItems,
              { type: "separator" },
              { role: "services" },
              { type: "separator" },
              { role: "hide" },
              { role: "hideOthers" },
              { role: "unhide" },
              { type: "separator" },
              { role: "quit" },
            ],
          },
        ]
      : []),
    {
      label: "File",
      submenu: [
        // A main-process accelerator, so it works with a native browser pane
        // focused. A focused `WebContentsView` swallows every keystroke — which
        // is why the palette's ⌘K has to be forwarded back to the page from
        // `browserViews.js` — but a menu accelerator is handled before the web
        // contents sees the key, so this one needs no forwarding.
        {
          label: "New Window",
          accelerator: "CmdOrCtrl+N",
          click: () => newWindowOrSayWhyNot(),
        },
        { type: "separator" },
        // On macOS this lives in the app menu, where the platform expects it.
        ...(isMac
          ? []
          : [
              {
                label: "Settings…",
                accelerator: "CmdOrCtrl+,",
                click: () => openSettings(),
              },
              { type: "separator" },
            ]),
        ...(isMac
          ? [{ role: "close" }]
          : [...veldItems, { type: "separator" }, { role: "about" }, { role: "quit" }]),
      ],
    },
    {
      label: "Edit",
      submenu: [
        { role: "undo" },
        { role: "redo" },
        { type: "separator" },
        { role: "cut" },
        { role: "copy" },
        { role: "paste" },
        { role: "selectAll" },
      ],
    },
    {
      label: "View",
      submenu: [
        { role: "reload" },
        { role: "forceReload" },
        { role: "toggleDevTools" },
        { type: "separator" },
        { role: "resetZoom" },
        { role: "zoomIn" },
        { role: "zoomOut" },
        { type: "separator" },
        { role: "togglefullscreen" },
      ],
    },
    {
      label: "Window",
      submenu: [
        { role: "minimize" },
        { role: "zoom" },
        ...(isMac ? [{ type: "separator" }, { role: "front" }] : [{ role: "close" }]),
      ],
    },
  ];
  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

app.on("second-instance", () => focusPrimary());

/**
 * Keep the daemon's version current for the skew notice. The reachability poll
 * in `loadAppWhenReady` stops as soon as the app loads, and `veld update` lands
 * long after that — so without this, a mismatch that appears (or is fixed) while
 * the app is open would never be seen. Loopback GET, so a minute is cheap.
 */
const VERSION_POLL_MS = 60_000;

app.whenReady().then(() => {
  if (!isPrimaryInstance) return;
  // Registered before any window exists, so the first page load already finds
  // the handlers. A view is only ever addressable from the window that owns it.
  registerBrowserViewIpc((event) => BrowserWindow.fromWebContents(event.sender), {
    // Global, like the window set beside it: a permission is granted to a site in
    // a session, and neither of those is per project.
    permissionsFile: path.join(app.getPath("userData"), "permissions.json"),
  });
  registerWindowIpc(ipcMain);
  buildAppMenu();
  app.setAboutPanelOptions({
    applicationName: "Veld",
    applicationVersion: app.getVersion(),
    copyright: "Prosperity Solutions",
  });
  initUpdater({
    onSkewChange: () => {
      buildAppMenu();
      void refreshTray?.();
    },
    // `quitAndInstall` can return without quitting, leaving the app running
    // after `before-quit` already latched. This is the only in-repo path that
    // genuinely cancels a quit, so it is the one that has to say so — and it is
    // the AppImage updater's, i.e. Linux, where `activate` never fires.
    onQuitCancelled: () => setQuitting(false),
  });
  setInterval(() => void daemonReachable(), VERSION_POLL_MS);
  // Unpackaged runs (`npm start`) show Electron's own icon in the dock, which
  // makes a dev window indistinguishable from any other Electron app. A packaged
  // build gets this from the bundle, so only set it when there is no bundle.
  if (process.platform === "darwin" && !app.isPackaged) {
    const icon = nativeImage.createFromPath(APP_ICON);
    if (!icon.isEmpty()) app.dock?.setIcon(icon);
  }
  // The windows the last run ended with, not just one: a detached window holds
  // live shells, and reopening only the main window would leave them running,
  // unreachable, until the detach grace hangs them up.
  restoreWindows();
  if (process.platform === "darwin") createTray();
  app.on("activate", () => {
    setQuitting(false);
    if (BrowserWindow.getAllWindows().length === 0) focusPrimary();
  });
});

/**
 * A quit is not a series of window closes.
 *
 * Every window's `close` runs on the way out, and without this flag each
 * detached one would try to hand its tabs to a window that is also closing, and
 * each `closed` would rewrite the persisted window set one window shorter —
 * so the next launch would reopen exactly one window and abandon the rest of
 * the layouts, with their shells, to the grace.
 */
app.on("before-quit", () => setQuitting(true));

/**
 * …and a `before-quit` is not always a quit — `quitAndInstall` can return
 * without quitting, and a macOS logout can be cancelled. A stuck latch is not
 * cosmetic, because `handBack` is gated on it too: every detached window closed
 * afterwards would abandon its tabs.
 *
 * The latch is therefore cleared only by callers that *know* the app is alive —
 * `openWindow`, `activate`, and `initUpdater`'s `onQuitCancelled` — never by
 * inferring it from a window event. Two versions tried to infer it and were
 * wrong in opposite directions; `setQuitting` in `windows.js` has that history.
 * The updater hook is the one that matters, since it is the only in-repo path
 * that genuinely cancels a quit and it is Linux's, where `activate` never fires.
 */

app.on("window-all-closed", () => {
  // Keep the tray alive on macOS (standard behavior); quit elsewhere.
  if (process.platform !== "darwin") app.quit();
});
