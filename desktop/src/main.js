// Veld Desktop — Electron shell around the veld daemon's /ide UI.
//
// Deliberately thin (see desktop/ARCHITECTURE.md): a frameless window that
// loads the daemon-served UI, a macOS tray with run status, and nothing else.
// The web UI must stay fully usable in a plain browser; everything the shell
// adds is presentation (native title bar) and ambient status (tray).

const {
  app,
  BrowserWindow,
  Menu,
  Tray,
  nativeImage,
  shell,
} = require("electron");

// Dev override: point the shell at the vite dev server
// (VELD_DESKTOP_URL=http://localhost:5199). Default: the daemon directly —
// no Caddy/helper needed.
const BASE_URL = process.env.VELD_DESKTOP_URL ?? "http://127.0.0.1:19899";
const APP_URL = `${BASE_URL}/ide?shell=electron`;
const HEALTH_URL = `${BASE_URL}/api/health`;
const ENVIRONMENTS_URL = `${BASE_URL}/api/environments`;

/** @type {BrowserWindow | null} */
let win = null;
/** @type {Tray | null} */
let tray = null;

// Shown while the daemon is unreachable; self-contained and branded
// (dark tokens + wordmark dot styling from the design handoff).
const WAITING_HTML = `<!doctype html><html><head><meta charset="utf-8"><title>Veld</title><style>
  body{margin:0;height:100vh;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:14px;
       background:#0d0e10;color:#98a0a9;font:13px/1.6 system-ui,sans-serif;-webkit-app-region:drag}
  .wm{font-weight:700;font-size:22px;color:#e7e9ec}.wm i{color:oklch(0.74 0.14 158);font-style:normal}
  code{font-family:ui-monospace,monospace;background:#1a1d21;border:1px solid #2a2e35;border-radius:6px;padding:2px 7px}
  p{max-width:380px;text-align:center;margin:0}
</style></head><body>
  <div class="wm">veld<i>.</i></div>
  <p>Waiting for the veld daemon…</p>
  <p>Install veld and run <code>veld doctor</code> if this is a fresh machine. Retrying automatically.</p>
</body></html>`;

async function daemonReachable() {
  try {
    const res = await fetch(HEALTH_URL, { signal: AbortSignal.timeout(2000) });
    return res.ok;
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
    width: 1280,
    height: 800,
    minWidth: 900,
    minHeight: 540,
    // Frameless with native traffic lights: the web UI renders veld controls
    // into the title-bar row (drag region handled in its CSS).
    titleBarStyle: "hiddenInset",
    backgroundColor: "#0d0e10",
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
  win.on("closed", () => {
    win = null;
  });
}

// 16×16 template icon (macOS renders it theme-aware): a "v" glyph drawn as
// a data-URI PNG would be blurry — use a simple vector-ish bitmap instead.
function trayIcon() {
  // 1-bit "v" in a 16x16 grid, generated at runtime to avoid a binary asset.
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
          running.push({ project: project.name, run });
        }
      }
    }
    items.push({
      label: running.length
        ? `${running.length} running run${running.length > 1 ? "s" : ""}`
        : "No running runs",
      enabled: false,
    });
    for (const { project, run } of running.slice(0, 10)) {
      items.push({
        label: `${project} / ${run.name} — ${run.status}`,
        click: () => focusWindow(),
      });
    }
  } catch {
    items.push({ label: "veld daemon unreachable", enabled: false });
  }
  items.push(
    { type: "separator" },
    { label: "Open Veld Desktop", click: () => focusWindow() },
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
  const refresh = async () => tray?.setContextMenu(await trayMenu());
  void refresh();
  setInterval(() => void refresh(), 10_000);
}

app.whenReady().then(() => {
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
