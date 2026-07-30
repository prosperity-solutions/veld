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
  nativeImage,
  shell,
} = require("electron");
const fs = require("node:fs");
const path = require("node:path");
const { registerBrowserViewIpc, disposeWindow } = require("./browserViews");
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
 * browser tab. `trayTemplate.png` is the menu-bar icon and carries the mark the
 * Hammerspoon widget also uses, so veld has one menu-bar identity; the
 * `Template` suffix is what makes macOS tint it for the current menu bar, which
 * is the only way one asset stays legible in light *and* dark mode.
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

const BASE_URL = process.env.VELD_DESKTOP_URL ?? "http://127.0.0.1:19899";
const APP_URL = `${BASE_URL}/ide?shell=electron`;
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

/** @type {BrowserWindow | null} */
let win = null;
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

// Shown while the daemon is unreachable; self-contained and branded
// (dark tokens + wordmark dot styling from the design handoff).
//
// The install command is spelled out rather than linked, because this screen is
// what a packaged download shows on a machine that has never had veld: the app
// is a shell around a daemon it does not ship, and "install veld" with no
// command is a dead end. `-webkit-user-select` is re-enabled on the command
// alone — the rest of the page is a drag region, which otherwise swallows the
// selection.
const INSTALL_COMMAND = "curl -fsSL https://veld.oss.life.li/get | bash";
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
  <p>On a fresh machine, install it with</p>
  <p><code class="cmd">${INSTALL_COMMAND}</code></p>
  <p>Already installed? Run <code>veld doctor</code>. Retrying automatically.</p>
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
    // A daemon old enough to have no version field is still reachable; the skew
    // check treats the unknown as "nothing to say".
    const body = await res.json().catch(() => null);
    noteDaemonVersion(body?.version);
    return true;
  } catch {
    return false;
  }
}

async function loadAppWhenReady(window) {
  if (await daemonReachable()) {
    await window.loadURL(APP_URL);
    return;
  }
  await window.loadURL(
    `data:text/html;charset=utf-8,${encodeURIComponent(WAITING_HTML)}`,
  );
  const timer = setInterval(async () => {
    if (window.isDestroyed()) {
      clearInterval(timer);
      return;
    }
    if (await daemonReachable()) {
      clearInterval(timer);
      await window.loadURL(APP_URL);
    }
  }, 2000);
}

function createWindow() {
  win = new BrowserWindow({
    // The window is titled by the app, not by the page: the UI is served from a
    // URL, so without this the title bar (and the macOS window menu, and Mission
    // Control) show whatever `<title>` the daemon's bundle happens to carry.
    title: "Veld",
    width: 1280,
    height: 800,
    minWidth: 900,
    minHeight: 540,
    // Frameless with native traffic lights: the web UI renders veld controls
    // into the title-bar row (drag region handled in its CSS).
    titleBarStyle: "hiddenInset",
    // Vertically centred in the top bar; `x` keeps hiddenInset's own inset.
    trafficLightPosition: {
      x: 13,
      y: Math.round((TOPBAR_HEIGHT - TRAFFIC_LIGHT_SIZE) / 2),
    },
    backgroundColor: "#0d0e10",
    // Windows/Linux take the window icon from here; macOS uses the bundle's (or
    // the dock icon set in `whenReady` while running unpackaged).
    icon: APP_ICON,
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      preload: require("node:path").join(__dirname, "preload.js"),
    },
  });

  // Run URLs open in the user's real browser, never inside the shell.
  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: "deny" };
  });

  // Same policy for top-level navigations (plain <a>, window.location,
  // redirects): the shell renders only the app origin; anything else goes to
  // the real browser. data: URLs (the waiting page) load via loadURL, which
  // doesn't emit will-navigate.
  const appOrigin = new URL(APP_URL).origin;
  win.webContents.on("will-navigate", (event, url) => {
    // Fail CLOSED: an unparseable target must not fall through into the
    // shell (skipping preventDefault would navigate).
    let origin = null;
    try {
      origin = new URL(url).origin;
    } catch {
      // leave origin null → blocked below
    }
    if (origin !== appOrigin) {
      event.preventDefault();
      if (origin) void shell.openExternal(url);
    }
  });

  void loadAppWhenReady(win);
  // Before `closed`: the window's `contentView` must still exist to detach the
  // browser panes from, and a view outliving its window keeps a renderer
  // process alive with nothing to paint into.
  // …and the page must not take it back. Electron adopts `document.title` on every
  // navigation unless the event is cancelled, which is how a hard reload renamed
  // the window.
  win.on("page-title-updated", (e) => e.preventDefault());

  win.on("close", () => {
    if (win) disposeWindow(win);
  });
  win.on("closed", () => {
    win = null;
  });
}

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
    // worktree emoji + alias — and only those, since for the single-checkout
    // majority it is noise, and then `/api/repos` isn't fetched at all.
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
        click: () => focusWindow(),
      });
    }
  } catch {
    items.push({ label: "veld daemon unreachable", enabled: false });
  }
  items.push(
    { type: "separator" },
    { label: "Open Veld Desktop", click: () => focusWindow() },
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

function focusWindow() {
  if (!win) createWindow();
  win?.show();
  win?.focus();
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
      submenu: isMac
        ? [{ role: "close" }]
        : [...veldItems, { type: "separator" }, { role: "about" }, { role: "quit" }],
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

app.on("second-instance", () => focusWindow());

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
  registerBrowserViewIpc((event) => BrowserWindow.fromWebContents(event.sender));
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
  });
  setInterval(() => void daemonReachable(), VERSION_POLL_MS);
  // Unpackaged runs (`npm start`) show Electron's own icon in the dock, which
  // makes a dev window indistinguishable from any other Electron app. A packaged
  // build gets this from the bundle, so only set it when there is no bundle.
  if (process.platform === "darwin" && !app.isPackaged) {
    const icon = nativeImage.createFromPath(APP_ICON);
    if (!icon.isEmpty()) app.dock?.setIcon(icon);
  }
  createWindow();
  if (process.platform === "darwin") createTray();
  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on("window-all-closed", () => {
  // Keep the tray alive on macOS (standard behavior); quit elsewhere.
  if (process.platform !== "darwin") app.quit();
});
