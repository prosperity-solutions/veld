const crypto = require("node:crypto");
const path = require("node:path");
const { pathToFileURL } = require("node:url");
const { createRemotePreview, runnerRendererURL } = require("./remotePreview");
const {
  RemoteConnectionError,
  createConnectionStore,
  createRemoteConnections,
} = require("./remoteConnections");

const CHANNEL_PREFIX = "veld:remote:";
const ACTIONS = ["list", "discover", "pair", "open", "forget"];
const MAX_REMOTE_WINDOWS = 8;
const MAX_PREVIEW_WINDOWS = 4;
const WINDOW_LOAD_TIMEOUT_MS = 15_000;

/** Persistent state belongs to this verified runner and paired registration. */
function remotePartition(connection) {
  const { descriptor, client_id, id } = connection;
  const identity = JSON.stringify(["veld-remote-ide-v1", descriptor.protocol, descriptor.instance_id,
    descriptor.public_key, descriptor.endpoint, client_id, id]);
  return `persist:veld-remote-ide-${crypto.createHash("sha256").update(identity).digest("hex")}`;
}

/** Compare HTTPS/WSS destinations without accepting credentials or sibling hosts/ports. */
function isRemoteOrigin(value, endpoint, allowWebSocket = false) {
  try {
    const url = new URL(value);
    if (url.username || url.password) return false;
    if (allowWebSocket && url.protocol === "wss:") url.protocol = "https:";
    return url.protocol === "https:" && url.origin === endpoint;
  } catch {
    return false;
  }
}

/**
 * Strip managed Authorization before deciding whether to add ours. Registered
 * previews keep their own application Authorization, never the runner token.
 */
function remoteRequestHeaders(details, { endpoint, credential, webContents, active, preview }) {
  const headers = Object.fromEntries(Object.entries(details.requestHeaders || {})
    .filter(([name, value]) => name.toLowerCase() !== "authorization" ||
      (active && preview?.isRegisteredURL(details.url) && !String(value).includes(credential))));
  if (active && !webContents.isDestroyed?.() && details.webContentsId === webContents.id && details.frame === webContents.mainFrame &&
      runnerRendererURL(details.url, endpoint, details.resourceType === "mainFrame")) {
    headers.Authorization = `Bearer ${credential}`;
  }
  return headers;
}

/** Privileged IPC accepts only the exact local chooser and its current main frame. */
function assertChooserSender(event, chooser, chooserUrl) {
  if (!chooser || chooser.isDestroyed() || event.sender !== chooser.webContents ||
      event.senderFrame !== chooser.webContents.mainFrame ||
      event.senderFrame?.url !== chooserUrl || chooser.webContents.getURL() !== chooserUrl) {
    throw new RemoteConnectionError("sender");
  }
}

function denyPermissions(partition) {
  partition.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
  partition.setPermissionCheckHandler(() => false);
  partition.setDevicePermissionHandler(() => false);
  partition.setDisplayMediaRequestHandler((_request, callback) => callback({}));
  partition.on("will-download", (event) => event.preventDefault());
}

function lockWindow(win, allows, preview = null, previewWindow = false) {
  const previewFrame = (url) => typeof url === "string" && url.startsWith("https:") && preview?.isPreviewURL(url);
  win.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
  win.webContents.on("will-attach-webview", (event) => event.preventDefault());
  win.webContents.on("will-navigate", (event, url) => {
    if (!allows(event.url ?? url)) event.preventDefault();
  });
  win.webContents.on("will-frame-navigate", (event) => {
    // The async request guard refreshes the registry before any preview bytes
    // leave this session, including a newly started service's first navigation.
    if (!allows(event.url) && !(event.isMainFrame === false && previewFrame(event.url))) event.preventDefault();
  });
  win.webContents.on("will-redirect", (event, url, _inPlace, isMainFrame) => {
    if (((event.isMainFrame ?? isMainFrame ?? true) && !previewWindow) || !previewFrame(event.url ?? url)) event.preventDefault();
  });
  win.webContents.on("login", (event, _details, authInfo, callback) => {
    event.preventDefault();
    const credentials = preview?.proxyCredentials(authInfo);
    if (credentials) callback(...credentials);
    else callback();
  });
}

/**
 * Create after app.whenReady(), before waiting for the local daemon.
 * register() installs the five chooser IPC handlers; openChooser() opens the
 * trusted local page. dispose() removes them and closes this manager's windows.
 * Electron dependencies and the service can be replaced by Node test doubles.
 */
function createRemoteWindows({ app, BrowserWindow, session, ipcMain, safeStorage, dialog,
  connections, tailscaleStatus, platform = process.platform, previewFactory = createRemotePreview }) {
  const chooserFile = path.join(__dirname, "remoteChooser.html");
  const chooserUrl = pathToFileURL(chooserFile).href;
  const windows = new Map();
  const pending = new Map();
  let chooser = null;
  let registered = false;
  let disposed = false;
  let mutation = false;
  let discovery = null;
  const controlSession = session.fromPartition(`veld-remote-control-${crypto.randomUUID()}`, { cache: false });
  denyPermissions(controlSession);
  const service = connections || createRemoteConnections({
    store: createConnectionStore({ userData: app.getPath("userData"), safeStorage, platform }),
    fetch: controlSession.fetch.bind(controlSession),
    tailscaleStatus,
  });

  function ensureActive() {
    if (disposed) throw new RemoteConnectionError("closed");
  }

  function focus(win) {
    if (win.isMinimized()) win.restore();
    win.show();
    win.focus();
  }

  async function openChooser() {
    ensureActive();
    if (chooser && !chooser.isDestroyed()) {
      focus(chooser);
      return chooser;
    }
    const chooserSession = session.fromPartition(`veld-remote-chooser-${crypto.randomUUID()}`, { cache: false });
    denyPermissions(chooserSession);
    const win = new BrowserWindow({
      title: "Remote connections — Veld", width: 720, height: 740, minWidth: 480, minHeight: 520,
      backgroundColor: "#0d0e10", show: false,
      webPreferences: {
        session: chooserSession, preload: path.join(__dirname, "remotePreload.js"),
        nodeIntegration: false, contextIsolation: true, sandbox: true, webviewTag: false,
      },
    });
    chooser = win;
    lockWindow(win, (url) => url === chooserUrl);
    win.on("closed", () => { if (chooser === win) chooser = null; });
    try {
      await win.loadFile(chooserFile);
      ensureActive();
      if (!win.isDestroyed()) focus(win);
      return win;
    } catch {
      if (!win.isDestroyed()) win.destroy();
      throw new RemoteConnectionError("closed");
    }
  }

  async function loadRemote(id) {
    const connection = await service.connect(id);
    ensureActive();
    if (windows.size >= MAX_REMOTE_WINDOWS) throw new RemoteConnectionError("windows");
    // Identity replacement cannot inherit a pending operation from an old
    // runner/client pair, even if a registration record keeps the same ID.
    const partition = session.fromPartition(remotePartition(connection), { cache: false });
    denyPermissions(partition);
    const title = `${connection.label} — ${connection.descriptor.endpoint} — ${connection.descriptor.instance_id} — Veld`;
    const win = new BrowserWindow({
      title, width: 1280, height: 860, minWidth: 640, minHeight: 480,
      backgroundColor: "#0d0e10", show: false,
      webPreferences: {
        session: partition, nodeIntegration: false, contextIsolation: true, sandbox: true,
        webviewTag: false, navigateOnDragDrop: false,
        // No preload: remote HTML has ordinary browser capabilities only.
      },
    });
    const auth = { endpoint: connection.descriptor.endpoint, credential: connection.credential,
      webContents: win.webContents, active: true };
    win.webContents.setWebRTCIPHandlingPolicy("disable_non_proxied_udp");
    let preview;
    try {
      preview = previewFactory({ session: partition, endpoint: connection.descriptor.endpoint,
        credential: connection.credential, fetch: controlSession.fetch.bind(controlSession),
        ideWebContentsId: win.webContents.id,
        onRevoked: () => {
          auth.active = false;
          auth.credential = "";
          if (!win.isDestroyed()) win.destroy();
        } });
    } catch (error) {
      auth.active = false;
      auth.credential = "";
      win.destroy();
      throw error;
    } finally {
      connection.credential = "";
    }
    auth.preview = preview;
    const record = { win, auth, preview, partition, title, previews: new Set() };
    windows.set(id, record);
    partition.webRequest.onBeforeSendHeaders((details, callback) => {
      callback({ requestHeaders: remoteRequestHeaders(details, auth) });
    });
    partition.webRequest.onHeadersReceived((details, callback) => {
      const redirect = details.statusCode >= 300 && details.statusCode < 400 && details.statusCode !== 304;
      let permitted = false;
      if (redirect && preview.isPreviewURL(details.url)) {
        const location = Object.entries(details.responseHeaders || {}).find(([name]) => name.toLowerCase() === "location")?.[1];
        try {
          const target = new URL(Array.isArray(location) ? location[0] : location, details.url).href;
          permitted = Boolean(location) && preview.isPreviewURL(target);
        } catch { permitted = false; }
      }
      callback({ cancel: redirect && !permitted });
    });
    lockWindow(win, (url) => runnerRendererURL(url, connection.descriptor.endpoint, true), preview);
    win.webContents.setWindowOpenHandler(({ url }) => {
      // Deny Chromium's popup and create our own window only after checking its
      // registration. The existing BrowserPane button already uses window.open.
      if (preview.isPreviewURL(url) && url.startsWith("https:")) {
        void openPreviewWindow(record, url).catch(() => {
          console.warn("[veld] remote preview window could not be opened");
        });
      }
      return { action: "deny" };
    });
    win.webContents.on("page-title-updated", (event) => event.preventDefault());
    win.on("closed", () => {
      auth.active = false;
      auth.credential = "";
      for (const child of record.previews) if (!child.isDestroyed()) child.destroy();
      preview.dispose();
      if (windows.get(id) === record) windows.delete(id);
      // Keep the stripping listener in place for any late network callbacks.
    });
    const timeout = setTimeout(() => { if (!win.isDestroyed()) win.destroy(); }, WINDOW_LOAD_TIMEOUT_MS);
    try {
      await preview.start();
      ensureActive();
      if (win.isDestroyed()) throw new RemoteConnectionError("closed");
      await win.loadURL(`${connection.descriptor.endpoint}/ide?remote=1`);
      ensureActive();
      if (win.isDestroyed()) throw new RemoteConnectionError("closed");
      focus(win);
      return { id };
    } catch {
      if (!win.isDestroyed()) win.destroy();
      throw new RemoteConnectionError("network");
    } finally {
      clearTimeout(timeout);
    }
  }

  async function openPreviewWindow(record, url) {
    if (disposed || !record.auth.active || record.win.isDestroyed() || record.previews.size >= MAX_PREVIEW_WINDOWS) return;
    const child = new BrowserWindow({
      title: `${new URL(url).host} — ${record.title}`, width: 1100, height: 800,
      backgroundColor: "#0d0e10", show: false,
      webPreferences: { session: record.partition, nodeIntegration: false, contextIsolation: true,
        sandbox: true, webviewTag: false, navigateOnDragDrop: false },
    });
    record.previews.add(child);
    const contentsId = child.webContents.id;
    child.webContents.setWebRTCIPHandlingPolicy("disable_non_proxied_udp");
    lockWindow(child, (target) => typeof target === "string" && target.startsWith("https:") && record.preview.isPreviewURL(target), record.preview, true);
    child.webContents.on("page-title-updated", (event) => event.preventDefault());
    child.on("closed", () => {
      record.previews.delete(child);
      record.preview.unregisterPreviewContents(contentsId);
    });
    const timeout = setTimeout(() => { if (!child.isDestroyed()) child.destroy(); }, WINDOW_LOAD_TIMEOUT_MS);
    try {
      record.preview.registerPreviewContents(contentsId);
      if (!(await record.preview.allowRequest({ url, webContentsId: contentsId, resourceType: "mainFrame" }))) {
        if (!child.isDestroyed()) child.destroy();
        return;
      }
      if (disposed || !record.auth.active || child.isDestroyed()) return;
      await child.loadURL(url);
      if (!child.isDestroyed() && record.auth.active) focus(child);
    } catch (error) {
      if (!child.isDestroyed()) child.destroy();
      throw error;
    } finally { clearTimeout(timeout); }
  }

  /** Open registered metadata by ID, verify again, then release its credential. */
  async function open(id) {
    ensureActive();
    const existing = windows.get(id);
    if (existing && !existing.win.isDestroyed()) {
      focus(existing.win);
      return { id };
    }
    if (pending.has(id)) return pending.get(id);
    const opening = loadRemote(id);
    pending.set(id, opening);
    try { return await opening; } finally { pending.delete(id); }
  }

  async function confirmPair(event, connection) {
    assertChooserSender(event, chooser, chooserUrl);
    if (!chooser.isVisible() || chooser.isMinimized()) throw new RemoteConnectionError("closed");
    const owner = chooser;
    const { response } = await dialog.showMessageBox(owner, {
      type: "warning", title: "Allow full control of this remote machine?",
      message: `Pair with ${connection.label}?`,
      detail: `This Desktop will have full control of Veld on the remote machine, including running commands and reading or changing files.\n\nAddress: ${connection.descriptor.endpoint}\nMachine: ${connection.descriptor.instance_id}\nPublic key: ${connection.descriptor.public_key}\n\nOnly continue if this invitation came from a machine you trust.`,
      buttons: ["Cancel", "Pair with full control"], defaultId: 0, cancelId: 0, noLink: true,
    });
    assertChooserSender(event, owner, chooserUrl);
    ensureActive();
    return response === 1 && chooser === owner && owner.isVisible() && !owner.isMinimized();
  }

  async function exclusive(action) {
    if (mutation) throw new RemoteConnectionError("busy");
    mutation = true;
    try { return await action(); } finally { mutation = false; }
  }

  const handlers = {
    list: () => service.list(),
    discover: async () => {
      if (discovery) return discovery;
      discovery = service.discover();
      try { return await discovery; } finally { discovery = null; }
    },
    pair: (event, invitation, label) => exclusive(() => service.pair(
      invitation, label, (connection) => confirmPair(event, connection),
    )),
    open: (_event, id) => exclusive(() => open(id)),
    forget: (_event, id) => exclusive(async () => {
      service.forget(id);
      const record = windows.get(id);
      if (record) {
        record.auth.active = false;
        record.auth.credential = "";
        record.win.destroy();
      }
      return { id };
    }),
  };

  function register() {
    ensureActive();
    if (registered) return;
    for (const action of ACTIONS) {
      ipcMain.handle(`${CHANNEL_PREFIX}${action}`, async (event, ...args) => {
        assertChooserSender(event, chooser, chooserUrl);
        try {
          ensureActive();
          return { ok: true, value: await handlers[action](event, ...args) };
        } catch (error) {
          // Never relay network/system exception messages: they may include
          // request bodies, headers, or decrypted credential material.
          const safe = error instanceof RemoteConnectionError ? error : new RemoteConnectionError("response");
          return { ok: false, error: { code: safe.code, message: safe.message } };
        }
      });
    }
    registered = true;
  }

  function dispose() {
    disposed = true;
    if (registered) for (const action of ACTIONS) ipcMain.removeHandler(`${CHANNEL_PREFIX}${action}`);
    registered = false;
    if (chooser && !chooser.isDestroyed()) chooser.destroy();
    for (const { win, auth } of windows.values()) {
      auth.active = false;
      auth.credential = "";
      if (!win.isDestroyed()) win.destroy();
    }
  }

  return { register, openChooser, open, dispose, owns: (contents) => Boolean(contents) &&
    (chooser?.webContents === contents || [...windows.values()].some(({ win, previews }) =>
      win.webContents === contents || [...previews].some((child) => child.webContents === contents))) };
}

module.exports = { createRemoteWindows, assertChooserSender, remoteRequestHeaders, isRemoteOrigin, remotePartition };
