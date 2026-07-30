// Bridge between the daemon-served /ide UI and the desktop shell.
//
// Two things only: a marker so the UI knows it runs inside Electron, and the
// embedded-browser surface (`browser`), which is the one feature the web build
// cannot do itself. Every method is a fixed channel with a fixed shape — the
// page never names a channel, so it cannot reach IPC handlers this file does
// not list. See desktop/src/browserViews.js for what the other side enforces.
const { contextBridge, ipcRenderer } = require("electron");

/** Subscribe to a main→renderer channel, returning an unsubscribe. */
function on(channel, fn) {
  const listener = (_event, payload) => fn(payload);
  ipcRenderer.on(channel, listener);
  return () => ipcRenderer.removeListener(channel, listener);
}

contextBridge.exposeInMainWorld("veldDesktop", {
  shell: "electron",
  version: process.versions.electron,
  browser: {
    /** Drop views left behind by a previous page. Called once, before any
     *  `create`, so the ordering is a queue rather than a race. */
    reset: () => ipcRenderer.invoke("veld:browser:reset"),
    /** Create (or adopt) the view for `viewId`; resolves to its state. */
    create: (viewId, options) =>
      ipcRenderer.invoke("veld:browser:create", { viewId, ...options }),
    /** Mirror the pane's rect, in CSS pixels relative to the window. */
    setBounds: (viewId, rect) => ipcRenderer.invoke("veld:browser:bounds", { viewId, rect }),
    setVisible: (viewId, visible) =>
      ipcRenderer.invoke("veld:browser:visible", { viewId, visible }),
    navigate: (viewId, url) => ipcRenderer.invoke("veld:browser:navigate", { viewId, url }),
    /** One of "back" | "forward" | "reload" | "stop" | "focus". */
    command: (viewId, command) =>
      ipcRenderer.invoke("veld:browser:command", { viewId, command }),
    /** Emulate a device, or `null` to show the pane at pane size. */
    emulate: (viewId, emulation) =>
      ipcRenderer.invoke("veld:browser:emulate", { viewId, emulation }),
    /** Page zoom factor for this pane (1 = 100%). */
    setZoom: (viewId, zoom) => ipcRenderer.invoke("veld:browser:zoom", { viewId, zoom }),
    /** One of "toggle" | "open" | "close". Always opens detached — a docked
     *  inspector and the renderer's bounds mirroring fight over the view's box. */
    devTools: (viewId, action) =>
      ipcRenderer.invoke("veld:browser:devtools", { viewId, action }),
    destroy: (viewId) => ipcRenderer.invoke("veld:browser:destroy", { viewId }),
    /** Clear one session slot's cookies and storage, pane or no pane. */
    clearSession: (profile) => ipcRenderer.invoke("veld:browser:clear-session", { profile }),
    /** A still of the view, so hiding it can freeze rather than blank the pane. */
    capture: (viewId) => ipcRenderer.invoke("veld:browser:capture", { viewId }),
    /** URL, title, loading and history state, pushed on every change. */
    onState: (fn) => on("veld:browser:state", fn),
    /** A `target=_blank` inside a pane: the UI decides where the tab opens. */
    onOpenRequest: (fn) => on("veld:browser:open-request", fn),
    /** An app accelerator a focused view would otherwise have swallowed. */
    onAccelerator: (fn) => on("veld:browser:accelerator", fn),
  },
});
