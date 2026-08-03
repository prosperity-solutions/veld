// Bridge between the daemon-served /ide UI and the desktop shell.
//
// Three things: a marker so the UI knows it runs inside Electron, the window
// surface (`window` — open, detach, hand back), and the embedded-browser surface
// (`browser`). The last two are the features the web build cannot do itself.
// Every method is a fixed channel with a fixed shape — the page never names a
// channel, so it cannot reach IPC handlers this file does not list. See
// desktop/src/windows.js and desktop/src/browserViews.js for what the other side
// enforces.
const { contextBridge, ipcRenderer } = require("electron");

/** A `--flag=value` the main process put on this renderer's command line. */
function fromArgv(flag) {
  const arg = process.argv.find((a) => a.startsWith(flag));
  return arg ? arg.slice(flag.length) : null;
}

/**
 * The layout this window boots with, when it was opened by pulling tabs out of
 * another one.
 *
 * **Synchronous on purpose.** The renderer prunes every terminal its layouts do
 * not name, so a seed that arrives one tick late reads as "these sessions are
 * orphans" and hangs up the shells that were just transferred — it has to be
 * readable in the first render. `sendSync` is what makes that possible without
 * putting it in argv, where it would be world-readable on Linux
 * (`/proc/<pid>/cmdline`) and would carry a browser pane's URL, fragment
 * included.
 *
 * Read here, at preload time — which happens on **every** load in this window,
 * the `data:` waiting page included. So this may well read the seed more than
 * once, and that is fine: the main process keeps it until the renderer reports
 * its first *snapshot*, i.e. until something else demonstrably holds these tabs.
 * Retiring it on the first read, or when the page finished loading, were both
 * tried and both lost a detach's tabs outright — see the `veld:window:seed`
 * handler in `windows.js`. Do not "simplify" either of them back.
 *
 * A later read is harmless anyway: by then the page's own storages hold the
 * layout and win over the seed (see `readLayouts`).
 */
function windowSeed() {
  try {
    const raw = ipcRenderer.sendSync("veld:window:seed");
    return typeof raw === "string" && raw !== "" ? raw : null;
  } catch {
    // An older or partly-initialised shell: no seed, which is the same state
    // every non-detached window is in.
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
    /** Whether the shell reopened this window on a slot it owned before. Only
     *  then may the page restore that slot's durable layout — see `readLayouts`. */
    restored: fromArgv("--veld-window-restored=") === "1",
    seed: windowSeed(),
    /** Open another full window. With no payload it inherits the app-wide last
     *  selection (what ⌘N does); with one it opens on that worktree. */
    newWindow: (payload) => ipcRenderer.invoke("veld:window:new", payload ?? {}),
    /** Ask to show a worktree. `{ok:false, reason:"shown-elsewhere"}` means
     *  another window has it and the shell just focused that window — the
     *  caller should stay where it is. */
    claimWorktree: (worktreeId, focusHolder = true) =>
      ipcRenderer.invoke("veld:window:claim", { worktreeId, focusHolder }),
    /** Which worktrees another window is showing right now — the rail greys
     *  those rows out. Answers the state this page booted into; changes arrive
     *  through `onClaimsChanged`. */
    claimedElsewhere: () => ipcRenderer.invoke("veld:window:claims"),
    /** …and every change to it, since they nearly all happen in some other
     *  window: a claim, a switch, a window closing. */
    onClaimsChanged: (fn) => on("veld:window:claims", fn),
    /** Which worktrees this window currently holds the panes of, so the shell
     *  knows who to ask to let go when another window claims one. */
    holdsWorktrees: (worktreeIds) =>
      ipcRenderer.invoke("veld:window:holds", { worktreeIds }),
    /** Let go of one worktree's panes — another window is taking it. Release,
     *  never close: the shells keep running for whoever attaches next. */
    onYieldWorktree: (fn) => on("veld:window:yield", fn),
    /** A tab drag started here. Every window freezes its embedded browser views
     *  (they paint over all DOM, so an overlay under one is invisible) and the
     *  shell starts carrying the cursor to whichever window it is over. */
    dragBegin: () => ipcRenderer.invoke("veld:window:drag-begin"),
    /** …and ended. Idempotent; `dropOut` ends it too. */
    dragEnd: () => ipcRenderer.invoke("veld:window:drag-end"),
    /** A tab drag began *somewhere* — freeze this window's views for it. */
    onDragBegin: (fn) => on("veld:window:drag-begin", fn),
    onDragEnd: (fn) => on("veld:window:drag-end", fn),
    /** The cursor is over this window, in its own content coordinates. Only the
     *  window under it hears this, and never the one the drag started in — that
     *  one has real drag events of its own. */
    onDragOver: (fn) => on("veld:window:drag-over", fn),
    /** …and has left again. */
    onDragOut: (fn) => on("veld:window:drag-out", fn),
    /** Tabs dropped on this window, to be placed where it was previewing. The
     *  page must answer with `dropApplied` — the window they came from does not
     *  let go until it does. */
    onDropHere: (fn) => on("veld:window:drop-here", fn),
    /** Which of them were actually placed. Anything omitted stays where it was. */
    dropApplied: (dropId, accepted) =>
      ipcRenderer.invoke("veld:window:drop-applied", { dropId, accepted }),
    /** A tab released outside this window, at a point in *screen* coordinates.
     *  The shell decides whether that lands on another Veld window (move the
     *  tabs there) or on nothing (open a new window) — the page cannot see
     *  either, since a drag never crosses a window boundary. */
    dropOut: (payload) => ipcRenderer.invoke("veld:window:drop-out", payload),
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
    /** Collect tabs handed back by detached windows that have closed. Call at
     *  mount *and* on the `onAdopt` nudge: the nudge can arrive before this
     *  page's listener exists, and the queue is what makes that survivable. */
    takeAdopted: () => ipcRenderer.invoke("veld:window:take-adopted"),
    /** A nudge — "there is something in your queue" — with no payload. */
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
