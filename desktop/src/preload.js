// Bridge between the daemon-served /ide UI and the desktop shell.
//
// Two things only: a marker so the UI knows it runs inside Electron, and the
// embedded-browser surface (`browser`), which is the one feature the web build
// cannot do itself. Every method is a fixed channel with a fixed shape — the
// page never names a channel, so it cannot reach IPC handlers this file does
// not list. See desktop/src/browserViews.js for what the other side enforces.
const { contextBridge, ipcRenderer } = require("electron");

/** A `--flag=value` the main process put on this renderer's command line. */
function fromArgv(flag) {
  const arg = process.argv.find((a) => a.startsWith(flag));
  return arg ? arg.slice(flag.length) : null;
}

/**
 * The layout this window boots with, when it was opened by pulling tabs out of
 * another one — base64 JSON, decoded here and parsed by the renderer.
 *
 * On the command line rather than in `localStorage`, because the seed has to be
 * readable *synchronously in the first render*: the renderer prunes every
 * terminal not named by its layouts, so a layout that arrives one tick late
 * reads as "these sessions are orphans" and hangs up the shells that were just
 * transferred. It is read once, at boot; from then on the window's own slot
 * store is the record (see `readLayouts`).
 */
function windowSeedFromArgv() {
  const raw = fromArgv("--veld-window-seed=");
  if (!raw) return null;
  try {
    return Buffer.from(raw, "base64").toString("utf8");
  } catch {
    return null;
  }
}

/** Subscribe to a main→renderer channel, returning an unsubscribe. */
function on(channel, fn) {
  const listener = (_event, payload) => fn(payload);
  ipcRenderer.on(channel, listener);
  return () => ipcRenderer.removeListener(channel, listener);
}

contextBridge.exposeInMainWorld("veldDesktop", {
  shell: "electron",
  version: process.versions.electron,
  /**
   * Which persisted pane layout this window owns (`main`, `dev`, …).
   *
   * On the bridge rather than in the URL because the renderer keys durable state
   * naming **live PTY sessions** off it: a query parameter can be forged by any
   * link, and a browser tab that claimed `main` would restore the desktop app's
   * terminals and fight it for them. Only a preload script can put this here.
   */
  layoutSlot: fromArgv("--veld-layout-slot="),
  /**
   * Windows: which one this is, and how a tab leaves or rejoins one.
   *
   * `kind` and `seed` come from argv for the same reason `layoutSlot` does —
   * they name live PTY sessions, and a URL is forgeable. `chrome=none` is
   * deliberately *not* here: it only hides UI, so it stays in the URL where it
   * is visible and debuggable in a plain browser.
   */
  window: {
    /** `"main"` (a full /ide) or `"detached"` (a bare dock). */
    kind: fromArgv("--veld-window-kind=") === "detached" ? "detached" : "main",
    seed: windowSeedFromArgv(),
    /** Open another full window, with its own worktree selection and layout. */
    newWindow: () => ipcRenderer.invoke("veld:window:new"),
    /** Pull tabs out into a window of their own. Resolves `{opened}` — the page
     *  must not remove them from its own layout until this says it worked. */
    detach: (payload) => ipcRenderer.invoke("veld:window:detach", payload),
    /** What this window would hand back if it closed now. Pushed on every layout
     *  change: `close` is not a moment a renderer can be asked anything. */
    snapshot: (payload) => ipcRenderer.invoke("veld:window:snapshot", payload),
    /** A detached window's title bar — the active tab, since there is no top bar
     *  in one to say what it holds. */
    setTitle: (title) => ipcRenderer.invoke("veld:window:set-title", { title }),
    /** Close this window (detached only): a bare dock with no tabs left in it. */
    close: () => ipcRenderer.invoke("veld:window:close"),
    /** Tabs handed back by a detached window that just closed. */
    onAdopt: (fn) => on("veld:window:adopt", fn),
  },
  browser: {
    /** Drop views left behind by a previous page. Called once, before any
     *  `create`, so the ordering is a queue rather than a race. */
    reset: () => ipcRenderer.invoke("veld:browser:reset"),
    /** Create (or adopt) the view for `viewId`; resolves to its state. */
    create: (viewId, options) =>
      ipcRenderer.invoke("veld:browser:create", { viewId, ...options }),
    /** Mirror the emulated screen's rect, in CSS pixels relative to the window,
     *  with the factor its viewport is rendered at inside it and the screen's
     *  corner radius. One call because they are one calculation: the page owns the
     *  geometry, the shell applies it. */
    setBounds: (viewId, rect, scale, radius) =>
      ipcRenderer.invoke("veld:browser:bounds", { viewId, rect, scale, radius }),
    setVisible: (viewId, visible) =>
      ipcRenderer.invoke("veld:browser:visible", { viewId, visible }),
    navigate: (viewId, url) => ipcRenderer.invoke("veld:browser:navigate", { viewId, url }),
    /** One of "back" | "forward" | "reload" | "stop" | "focus". */
    command: (viewId, command) =>
      ipcRenderer.invoke("veld:browser:command", { viewId, command }),
    /** Emulate a device, or `null` to show the pane at pane size. */
    emulate: (viewId, emulation) =>
      ipcRenderer.invoke("veld:browser:emulate", { viewId, emulation }),
    /** Repaint every pane view on the page's theme surface — what shows before a guest
     *  paints. Window-wide: a theme switch is one event for the whole app. */
    setBackground: (background) =>
      ipcRenderer.invoke("veld:browser:background", { background }),
    /** Page zoom factor for this pane (1 = 100%). */
    setZoom: (viewId, zoom) => ipcRenderer.invoke("veld:browser:zoom", { viewId, zoom }),
    /** One of "toggle" | "open" | "close". Always opens detached — a docked
     *  inspector and the renderer's bounds mirroring fight over the view's box. */
    devTools: (viewId, action) =>
      ipcRenderer.invoke("veld:browser:devtools", { viewId, action }),
    /** Ask the shell to forward the window's pane pointers while the page drags a
     *  screen's edge. Without it a drag dies the moment the cursor crosses a view,
     *  which owns every mouse event inside its own rect. Window-wide and view-less on
     *  purpose: the release often lands on a *different* pane, and the disarm has to
     *  work after the dragged pane is gone. */
    drag: (dragging) => ipcRenderer.invoke("veld:browser:drag", { dragging }),
    /** Mouse moves and the mouse-up seen *by the pane's page*, in the window's CSS
     *  pixels — only while `drag` is on. */
    onPointer: (fn) => on("veld:browser:pointer", fn),
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
