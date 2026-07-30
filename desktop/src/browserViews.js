// Embedded browser panes — the Electron half.
//
// The /ide UI can host a "browser" pane: a live web view of one of the run's
// URLs, inside the dock, with its own chrome (address bar, back/forward,
// profile). In the desktop shell that view is a real `WebContentsView` owned by
// this process; in a plain browser the UI falls back to an `<iframe>` (see
// `crates/veld-daemon/ui/src/panes/browserHost.ts`). Two backends because
// "the web UI must stay fully usable without Electron" is an invariant
// (desktop/ARCHITECTURE.md), and an iframe cannot do isolated cookie jars,
// cross-origin history, or pages that refuse framing.
//
// A `WebContentsView` is a **native sibling** of the renderer, not a DOM node:
// it does not participate in z-order or layout. So the renderer owns the
// geometry — it reports the pane's rect and this module mirrors it — and the
// renderer is also responsible for hiding views while a menu or dialog would be
// painted underneath one. Everything that arrives here is treated as untrusted
// input from the page: ids, profile names and URLs are all validated below.
//
// Ownership: views are keyed by (window id, view id), and every handler resolves
// the sender to a window's own **main frame** (`senderWindow`), so a renderer can
// only address its own views. Closing a window disposes its set. Note that the
// load-bearing isolation today is that pane views get no preload at all — pane
// content cannot reach this IPC surface in the first place; the keying and the
// frame check are what keep that true if one ever gains a preload.

const { WebContentsView, ipcMain, session } = require("electron");
// The trust boundary lives in its own tested module — see src/validate.js.
const {
  fitScale,
  isProfileName,
  isViewId,
  partitionFor,
  safeEmulation,
  safeUrl,
  safeZoom,
} = require("./validate");

/**
 * Per-window view cap.
 *
 * Each view is a full renderer process with its own cookie jar; a page that
 * loops `create` would otherwise be a memory bomb. High enough that no real
 * layout hits it.
 */
const MAX_VIEWS_PER_WINDOW = 16;

/**
 * @typedef {{width: number, height: number, deviceScaleFactor: number, mobile: boolean,
 *            touch: boolean, userAgent: string|null, fit: boolean}} Emulation
 */

/**
 * @typedef {{view: import('electron').WebContentsView, profile: string, visible: boolean,
 *            emulation: Emulation|null, zoom: number, defaultUserAgent: string,
 *            bounds: {x: number, y: number, width: number, height: number}|null,
 *            scale: number, touchActive: boolean}} Entry
 */

/** window.id → (viewId → Entry) */
/** @type {Map<number, Map<string, Entry>>} */
const byWindow = new Map();

function entriesFor(windowId) {
  const existing = byWindow.get(windowId);
  if (existing) return existing;
  const fresh = new Map();
  byWindow.set(windowId, fresh);
  return fresh;
}

/**
 * The state the pane's chrome renders. Read from the view's `webContents` on
 * every change rather than tracked incrementally, so it cannot drift from what
 * the view is actually showing.
 */
/**
 * @typedef {{kind: "load"|"cert"|"crash", code: number|null, text: string, url: string}} ViewError
 */

function stateOf(viewId, entry, error) {
  const wc = entry.view.webContents;
  // A crashed renderer still emits an event, and every getter below throws on a
  // destroyed WebContents — so the pane would lose the one message that explains
  // why it is blank.
  if (wc.isDestroyed()) {
    return {
      viewId,
      url: "",
      title: "",
      loading: false,
      canGoBack: false,
      canGoForward: false,
      profile: entry.profile,
      emulationScale: 1,
      touchActive: false,
      devToolsOpen: false,
      ...(error === undefined ? {} : { error }),
    };
  }
  return {
    viewId,
    url: wc.getURL(),
    title: wc.getTitle(),
    loading: wc.isLoading(),
    canGoBack: wc.navigationHistory.canGoBack(),
    canGoForward: wc.navigationHistory.canGoForward(),
    profile: entry.profile,
    // The three things the pane cannot work out for itself. It knows what it
    // *asked* for — the emulation and the zoom live in its layout — but not how
    // far a fitted viewport ended up scaled, nor whether touch is actually in
    // force, which DevTools takes away (see [`applyTouch`]). Flat primitives on
    // purpose: the renderer's `patch` compares values with `!==`, so a nested
    // object would count as changed on every event and re-render every pane.
    emulationScale: entry.scale,
    touchActive: entry.touchActive,
    devToolsOpen: wc.isDevToolsOpened(),
    ...(error === undefined ? {} : { error }),
  };
}

function send(window, channel, payload) {
  if (window.isDestroyed() || window.webContents.isDestroyed()) return;
  window.webContents.send(channel, payload);
}

function pushState(window, viewId, entry) {
  send(window, "veld:browser:state", stateOf(viewId, entry));
}

// ---------------------------------------------------------------------------
// Device emulation, zoom and DevTools
// ---------------------------------------------------------------------------
//
// A pane is small and a desktop layout is not, which is the whole reason this
// is worth having *inside* the dock: emulating a 1440-wide viewport scaled down
// to fit a 600px pane is the one case a real browser window cannot give you
// without a second monitor.
//
// Three rules hold this together, each of which is a bug if broken:
//
// 1. **The state lives in the renderer's layout, not here.** Emulation, zoom and
//    UA are per-`WebContents`, and a pane switching session destroys and
//    recreates its view — so everything below is re-asserted from the `create`
//    payload, exactly as the URL is. This module is the applier, not the owner.
// 2. **Zoom is re-asserted after every navigation.** Chromium's zoom policy is
//    per *origin*, so navigating adopts whatever that origin was last viewed at,
//    in *any* pane sharing the session. Without re-assertion a pane's zoom
//    silently changes when you follow a link, and setting one pane's zoom moves
//    its neighbour's.
// 3. **Touch is the one thing that can be taken away.** It needs a CDP session
//    (`Emulation.setEmitTouchEventsForMouse` has no Electron API), and Chromium
//    gives the built-in DevTools that session exclusively: opening DevTools
//    detaches our debugger. So `touchActive` is reported separately from the
//    `touch` the pane asked for, and re-attached when DevTools closes.

/**
 * Push the emulated viewport onto the view.
 *
 * `scale` is computed here rather than sent by the renderer because it is a
 * function of the view's box in **device-independent pixels**, and only this
 * process knows that number: page zoom scales the CSS pixels the renderer
 * measures and not the bounds a native view is given. Which is also why this
 * runs again from the bounds handler — a fitted viewport's scale is a function of
 * the pane's size, so resizing the dock has to recompute it.
 */
function applyMetrics(entry) {
  const wc = entry.view.webContents;
  if (wc.isDestroyed()) return;
  const emulation = entry.emulation;
  if (!emulation) {
    entry.scale = 1;
    wc.disableDeviceEmulation();
    return;
  }
  entry.scale = fitScale(emulation, entry.bounds);
  wc.enableDeviceEmulation({
    // `mobile` is Chromium's own word for viewport-meta handling, overlay
    // scrollbars and text autosizing — not for touch, which is separate below.
    screenPosition: emulation.mobile ? "mobile" : "desktop",
    screenSize: { width: emulation.width, height: emulation.height },
    viewSize: { width: emulation.width, height: emulation.height },
    viewPosition: { x: 0, y: 0 },
    deviceScaleFactor: emulation.deviceScaleFactor,
    scale: entry.scale,
  });
}

/**
 * Set or restore the user agent.
 *
 * The default is read off the view at creation rather than reconstructed: it is
 * the session's, which depends on the Chromium and Electron the app was built
 * with, and getting it wrong means every pane that ever emulated a phone keeps
 * claiming to be one after emulation is switched off.
 */
function applyUserAgent(entry) {
  const wc = entry.view.webContents;
  if (wc.isDestroyed()) return;
  wc.setUserAgent(entry.emulation?.userAgent ?? entry.defaultUserAgent);
}

/** Re-assert the pane's zoom. See rule 2 above for why this is not a one-shot. */
function applyZoom(entry) {
  const wc = entry.view.webContents;
  if (wc.isDestroyed()) return;
  wc.setZoomFactor(entry.zoom);
}

/**
 * Turn touch emulation on or off, and report what actually happened.
 *
 * Two CDP calls, because they answer different questions and a page asks both:
 * `setTouchEmulationEnabled` is what makes `ontouchstart` exist and
 * `navigator.maxTouchPoints` non-zero (feature detection), while
 * `setEmitTouchEventsForMouse` is what turns a drag into `touchstart`/`touchmove`
 * (the behaviour). Only the pair makes a swipe gesture testable.
 *
 * `attach` throws when a debugger is already attached — which is exactly what the
 * built-in DevTools is — so a failure here is a normal state, not an error worth
 * raising on the pane. It is reported as `touchActive: false` and retried when
 * DevTools closes.
 */
async function applyTouch(window, viewId, entry) {
  const wc = entry.view.webContents;
  if (wc.isDestroyed()) return;
  const wanted = entry.emulation?.touch === true;
  const dbg = wc.debugger;

  if (!wanted) {
    if (entry.touchActive && dbg.isAttached()) {
      // Explicitly off before detaching. Detaching *should* drop the session's
      // overrides on its own, but "should" is the wrong footing for a page that
      // would otherwise keep answering mouse drags with touch events.
      try {
        await dbg.sendCommand("Emulation.setEmitTouchEventsForMouse", { enabled: false });
        await dbg.sendCommand("Emulation.setTouchEmulationEnabled", { enabled: false });
      } catch {
        // The session went away underneath us — which is the state we wanted.
      }
    }
    if (dbg.isAttached()) {
      try {
        dbg.detach();
      } catch {
        // Already gone.
      }
    }
    if (entry.touchActive) {
      entry.touchActive = false;
      pushState(window, viewId, entry);
    }
    return;
  }

  try {
    if (!dbg.isAttached()) dbg.attach("1.3");
    await dbg.sendCommand("Emulation.setTouchEmulationEnabled", {
      enabled: true,
      maxTouchPoints: 1,
    });
    await dbg.sendCommand("Emulation.setEmitTouchEventsForMouse", {
      enabled: true,
      configuration: "mobile",
    });
    if (!entry.touchActive) {
      entry.touchActive = true;
      pushState(window, viewId, entry);
    }
  } catch {
    // DevTools holds the session, or the view is tearing down. The pane shows
    // touch as suspended rather than as on.
    if (entry.touchActive) {
      entry.touchActive = false;
      pushState(window, viewId, entry);
    }
  }
}

/**
 * Whether a change of emulation needs the page reloaded to be visible.
 *
 * Size and scale are live — Chromium relays out the emulated viewport and the
 * page's media queries follow. The user agent and touch support are not: a
 * document read `navigator.userAgent` and tested for `ontouchstart` while it was
 * loading, and every library that branched on either did so once. Picking
 * "iPhone" and being handed the desktop page is the confusing half of that, so
 * those two reload and the rest do not.
 */
function emulationNeedsReload(prev, next) {
  const ua = (e) => e?.userAgent ?? null;
  const touch = (e) => e?.touch === true;
  return ua(prev) !== ua(next) || touch(prev) !== touch(next);
}

/**
 * Apply everything an emulation controls, in the order a fresh view needs it.
 *
 * Metrics before UA before touch, and all of it before the first `loadURL`, so
 * the page's *first* request carries the emulated UA and its first script sees
 * the touch API. A later change reloads instead (see [`emulationNeedsReload`]).
 */
function applyEmulation(window, viewId, entry) {
  applyMetrics(entry);
  applyUserAgent(entry);
  void applyTouch(window, viewId, entry);
}

/**
 * Wire the events the pane chrome needs.
 *
 * `error` is sticky per navigation: it is set by a failed load and cleared by
 * the next one that starts, so the pane can show *why* it is blank instead of
 * just being blank.
 */
function attachListeners(window, viewId, entry) {
  const wc = entry.view.webContents;
  const push = (error) => send(window, "veld:browser:state", stateOf(viewId, entry, error));

  wc.on("did-start-loading", () => push(null));
  wc.on("did-stop-loading", () => push());
  wc.on("page-title-updated", () => push());
  wc.on("did-navigate", () => push());
  wc.on("did-navigate-in-page", () => push());

  // Errors go over the wire **structured**, not as a sentence. The pane picks the
  // icon and the wording from the code — "nothing is listening" and "that
  // hostname does not resolve" are different problems with different fixes, and a
  // formatted string forces the renderer to parse prose to tell them apart.
  wc.on("did-fail-load", (_e, code, description, validatedURL, isMainFrame) => {
    // Subframe failures are the page's business, and `-3` is ABORTED — which is
    // what a user-cancelled or superseded navigation reports, not a fault.
    if (!isMainFrame || code === -3) return;
    push({ kind: "load", code, text: description || "load failed", url: validatedURL });
  });

  // Veld serves runs over HTTPS with Caddy's local CA. If that CA is not in the
  // system trust store the load fails here — report it and point at the fix.
  // Never `event.preventDefault()`: silently trusting a bad certificate inside
  // an embedded view is exactly the hole this pane must not open.
  // Main frame only — `isMainFrame` is the 6th parameter of the *webContents*
  // form of this event (the 7-parameter form with a `webContents` argument is
  // `app.on`). A subresource with a bad certificate inside a page that rendered
  // fine would otherwise raise a full-pane error screen, which `covered()` in the
  // renderer answers by hiding the live view — and it would stick, because the
  // error is only cleared by `did-start-loading`, which a subresource failure
  // never fires. Nothing is lost by filtering: a main-frame certificate failure
  // also arrives as `did-fail-load` with −200..−299, which `describeBrowserError`
  // already maps to `cert`.
  wc.on("certificate-error", (_e, url, error, _certificate, _callback, isMainFrame) => {
    if (!isMainFrame) return;
    push({ kind: "cert", code: null, text: String(error), url });
  });

  wc.on("render-process-gone", (_e, details) => {
    push({ kind: "crash", code: null, text: String(details.reason), url: "" });
  });

  // A `target=_blank` (or `window.open`) inside a pane becomes another browser
  // *tab in the same dock*, carrying the same profile — so the popup keeps the
  // session it was opened from. The renderer decides where the tab goes; if it
  // ignores the request nothing opens, which is the safe direction. Non-http
  // targets go to the real browser, matching the main window's policy.
  entry.view.webContents.setWindowOpenHandler(({ url }) => {
    const safe = safeUrl(url);
    if (safe) {
      send(window, "veld:browser:open-request", { viewId, url: safe, profile: entry.profile });
    }
    // Nothing else is launched. A `mailto:`/`tel:` here would hand a
    // page-supplied string to `shell.openExternal` with no user gesture and no
    // confirmation, so a previewed page could open prefilled Mail or FaceTime
    // drafts in a loop that look like the user composed them. A preview of a dev
    // server has no business launching applications.
    return { action: "deny" };
  });

  // Top-level navigation is the *user's* browsing, so http(s) is allowed
  // wherever it goes — but the scheme filter still applies, and it must fail
  // closed: a target we cannot parse is blocked rather than followed.
  wc.on("will-navigate", (event, url) => {
    if (!safeUrl(url)) event.preventDefault();
  });

  // While a native view has keyboard focus the renderer sees no keys at all, so
  // the app's own accelerators are dead the moment you click into a preview.
  // Only the command palette's binding is intercepted and forwarded:
  // `Ctrl/⌘+Shift+P` is the app's documented one, and unlike `⌘K` it is not
  // something the previewed page is likely to want for itself.
  wc.on("before-input-event", (event, input) => {
    if (input.type !== "keyDown" || !input.shift) return;
    if (!(input.control || input.meta)) return;
    if (input.key.toLowerCase() !== "p") return;
    event.preventDefault();
    // Move the keyboard back to the page first: forwarding the accelerator
    // alone would open the palette while every keystroke still went to the
    // view, so the input would be there and unusable.
    window.webContents.focus();
    send(window, "veld:browser:accelerator", { viewId, accelerator: "palette" });
  });

  // Re-assert the pane's zoom and its touch emulation after every navigation.
  //
  // Zoom because Chromium's zoom policy is **per origin**: a navigation adopts
  // whatever the destination origin was last viewed at — including a level set by
  // a *different* pane on the same session — so a pane that does not re-assert
  // changes zoom on its own as you browse. Touch because the CDP overrides are
  // tied to the session and a cross-origin navigation can replace the render
  // frame underneath them; re-sending is cheap and idempotent, and `applyTouch`
  // only pushes state when the answer actually changes.
  wc.on("did-navigate", () => {
    applyZoom(entry);
    void applyTouch(window, viewId, entry);
  });

  // Opening DevTools takes the debugger session away — Chromium allows one
  // client, and the built-in inspector wins. So touch is *suspended*, not
  // broken, and the pane is told which of the two it is.
  wc.on("devtools-opened", () => push());
  wc.on("devtools-closed", () => {
    push();
    void applyTouch(window, viewId, entry);
  });
  wc.debugger.on("detach", () => {
    if (!entry.touchActive) return;
    entry.touchActive = false;
    push();
  });

  // No device access from an embedded preview. Notifications, camera, mic,
  // geolocation and the rest would be granted against the *pane's* origin with no
  // chrome to attribute the prompt to, so the whole set is denied.
  //
  // Both handlers, not just the request one: Electron's own docs say "you must
  // also implement setPermissionRequestHandler to get complete permission
  // handling. Most web APIs do a permission check and then make a permission
  // request if the check is denied" — and four permissions appear in the *check*
  // union and never in the request one (`deprecated-sync-clipboard-read`, `hid`,
  // `serial`, `usb`). Sync clipboard read is the concrete one: without a check
  // handler a previewed page could `document.execCommand("paste")` and read
  // whatever the user last copied, with no prompt.
  wc.session.setPermissionRequestHandler((_wc, _permission, callback) => callback(false));
  wc.session.setPermissionCheckHandler(() => false);
}

function disposeEntry(window, entry) {
  const wc = entry.view.webContents;
  // Before the view goes: a detached inspector is a window of its own, and one
  // left open for a pane that no longer exists is a window with no way back to
  // the app that opened it. The debugger goes with it, so a closing pane cannot
  // leave a CDP session attached to a dying WebContents.
  if (!wc.isDestroyed()) {
    if (wc.isDevToolsOpened()) wc.closeDevTools();
    if (wc.debugger.isAttached()) {
      try {
        wc.debugger.detach();
      } catch {
        // Already detached, or the target is gone.
      }
    }
  }
  try {
    window.contentView.removeChildView(entry.view);
  } catch {
    // Window already tearing down — nothing to detach from.
  }
  if (!wc.isDestroyed()) wc.close();
}

/** Drop every view a window owns. Called when the window closes. */
function disposeWindow(window) {
  const entries = byWindow.get(window.id);
  if (!entries) return;
  for (const entry of entries.values()) disposeEntry(window, entry);
  byWindow.delete(window.id);
}

/**
 * Register the IPC surface. Call once, before any window is created.
 *
 * `resolveWindow` maps an IPC event to the window that may own views — the
 * caller's own `BrowserWindow`. Handlers that cannot resolve one do nothing:
 * a message from a destroyed window is a race, not an error worth throwing at
 * a renderer that is already gone.
 */
function registerBrowserViewIpc(resolveWindow) {
  /**
   * The window whose views this sender may address, or null.
   *
   * The main-frame assertion is what the (window id, view id) keying alone does
   * not give: today the isolation rests on pane views having no preload, so no
   * pane content can reach IPC at all. The moment one gets a preload — injecting
   * veld's own overlay into a previewed page is the obvious next increment —
   * arbitrary web content would inherit its host window's whole view set,
   * including its siblings' cookie jars. Electron recommends this check for
   * exactly that reason.
   */
  const senderWindow = (event) => {
    const window = resolveWindow(event);
    if (!window) return null;
    if (event.senderFrame !== window.webContents.mainFrame) return null;
    return window;
  };

  /** Resolve (window, entry) for an addressed view, or null. */
  const lookup = (event, viewId) => {
    const window = senderWindow(event);
    if (!window || typeof viewId !== "string") return null;
    const entry = byWindow.get(window.id)?.get(viewId);
    return entry ? { window, entry } : null;
  };

  /**
   * Drop every view this window owns.
   *
   * Called by the page as it boots, *before* it creates anything. That ordering
   * is the whole point: a reload replaces the page's registry of views, so the
   * old ones are orphans painting over the new document with nothing able to
   * address them — but disposing them from a navigation event in this process is a
   * race against the renderer's first `create`, and losing it destroyed the view
   * the new page had just made. Blank pane, and nothing to do but reload. Driving
   * it from the renderer makes the ordering a queue instead of a race.
   */
  ipcMain.handle("veld:browser:reset", (event) => {
    const window = senderWindow(event);
    if (window) disposeWindow(window);
  });

  ipcMain.handle("veld:browser:create", (event, args) => {
    const window = senderWindow(event);
    if (!window) return null;
    const viewId = args?.viewId;
    if (!isViewId(viewId)) throw new Error("invalid view id");
    const profile = typeof args?.profile === "string" ? args.profile : "default";
    if (!isProfileName(profile)) throw new Error("invalid profile name");

    const entries = entriesFor(window.id);
    const existing = entries.get(viewId);
    if (existing) {
      // Idempotent: a remount must adopt the live view, or the pane would lose
      // its page on every tab switch. A profile change needs a new view, which
      // the renderer does by destroying this one first (and it changes the tab
      // id, so this branch is not the profile-switch path).
      return stateOf(viewId, existing);
    }
    if (entries.size >= MAX_VIEWS_PER_WINDOW) {
      throw new Error(`too many browser panes (max ${MAX_VIEWS_PER_WINDOW})`);
    }

    const view = new WebContentsView({
      webPreferences: {
        // The pane shows arbitrary web content: no preload, no node, sandboxed,
        // and a cookie jar of its own.
        partition: partitionFor(profile),
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: true,
        webSecurity: true,
        // Opened by us, never by the page, so nothing needs a window reference.
        webviewTag: false,
      },
    });
    view.setBackgroundColor("#ffffff");
    // Created **visible**, and only hidden when the renderer says so. A hidden
    // WebContents is background-throttled by Chromium, and a view created hidden
    // and loaded in the same tick sometimes never rendered its first page — blank
    // until you hit Reload. The renderer sends its own `visible` immediately, so
    // starting visible costs nothing.
    const entry = {
      view,
      profile,
      visible: true,
      // Emulation and zoom arrive with `create` rather than in a follow-up call,
      // because a pane switching session recreates its view and a device that
      // came back one round trip late would be visible as the page laying out at
      // pane size and then jumping.
      emulation: safeEmulation(args?.emulation),
      zoom: safeZoom(args?.zoom) ?? 1,
      // The session's own UA, captured before anything overrides it — what
      // "no emulated device" has to restore.
      defaultUserAgent: view.webContents.getUserAgent(),
      bounds: null,
      scale: 1,
      touchActive: false,
    };
    entries.set(viewId, entry);
    window.contentView.addChildView(view);
    attachListeners(window, viewId, entry);
    // Before the first load, so the page's first request carries the emulated UA
    // and its first script sees the touch API — the alternative is a reload.
    applyEmulation(window, viewId, entry);
    applyZoom(entry);

    const url = safeUrl(args?.url);
    if (url) void view.webContents.loadURL(url);
    return stateOf(viewId, entry);
  });

  ipcMain.handle("veld:browser:bounds", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const r = args?.rect;
    if (!r) return;
    // The renderer measures in CSS pixels; `setBounds` is in DIP relative to the
    // window's content view. Page zoom scales one and not the other, so without
    // this a zoomed /ide puts every native view over the wrong region — which is
    // the failure the renderer's geometry mirroring exists to prevent. Electron's
    // default application menu supplies ⌘+/⌘− (this app never replaces it), so
    // the zoom factor is one keystroke from being anything.
    const zoom = event.sender.getZoomFactor();
    const rect = {
      x: Math.round(Number(r.x) * zoom),
      y: Math.round(Number(r.y) * zoom),
      width: Math.round(Number(r.width) * zoom),
      height: Math.round(Number(r.height) * zoom),
    };
    if (!Object.values(rect).every(Number.isFinite)) return;
    // A zero or negative box is what a hidden or mid-layout pane reports; keep
    // the last good bounds and let visibility do the hiding, so returning to
    // the tab doesn't flash a 1px view.
    if (rect.width < 1 || rect.height < 1) return;
    found.entry.view.setBounds(rect);
    found.entry.bounds = rect;
    // A fitted viewport's scale is a function of the pane's size, so resizing the
    // dock has to recompute it — and only when it actually moves, because the
    // renderer coalesces a splitter drag to one push per frame and re-emulating
    // on each one would relayout the page per frame. The scale also goes back to
    // the pane, which is what lets the chrome show "1440 × 900 at 42%".
    if (found.entry.emulation?.fit) {
      const before = found.entry.scale;
      applyMetrics(found.entry);
      if (found.entry.scale !== before) pushState(found.window, args.viewId, found.entry);
    }
  });

  /**
   * Set (or clear) the pane's emulated device.
   *
   * `null` is a first-class argument, not a missing one: "show this pane at pane
   * size" is a state the user picks, and it has to reach `disableDeviceEmulation`
   * and restore the default user agent.
   */
  ipcMain.handle("veld:browser:emulate", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const { window, entry } = found;
    const wc = entry.view.webContents;
    if (wc.isDestroyed()) return;
    const next = safeEmulation(args?.emulation);
    const prev = entry.emulation;
    entry.emulation = next;
    applyEmulation(window, args.viewId, entry);
    // The size is live; the UA and touch support are not — a document read them
    // while it was loading. Reload only for those, and only when there is a page
    // to reload: a blank pane has nothing to re-request, and `reload()` on one
    // would report a navigation the pane never made.
    if (emulationNeedsReload(prev, next) && wc.getURL()) wc.reload();
    pushState(window, args.viewId, entry);
  });

  /** Page zoom for one pane. Chromium stores zoom per origin, so this is
   *  re-asserted after every navigation (see `attachListeners`). */
  ipcMain.handle("veld:browser:zoom", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const zoom = safeZoom(args?.zoom);
    if (zoom === null) return;
    found.entry.zoom = zoom;
    applyZoom(found.entry);
  });

  /**
   * Open or close this pane's DevTools.
   *
   * **Detached is the only workable mode.** A docked inspector resizes the view
   * from the inside while the renderer mirrors the pane's box from the outside,
   * and the two fight: every resize the inspector makes is undone by the next
   * `setBounds`, which arrives on a 400 ms tick.
   *
   * The debugger is detached *first*, rather than waiting for Chromium to do it:
   * touch emulation is about to lose its session either way, and doing it here
   * means the pane learns that touch is suspended in the same round trip that
   * opened the inspector.
   */
  ipcMain.handle("veld:browser:devtools", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const { window, entry } = found;
    const wc = entry.view.webContents;
    if (wc.isDestroyed()) return;
    const action = args?.action;
    const open = action === "open" || (action === "toggle" && !wc.isDevToolsOpened());
    if (open) {
      if (entry.touchActive) {
        entry.touchActive = false;
        if (wc.debugger.isAttached()) {
          try {
            wc.debugger.detach();
          } catch {
            // Already gone; the inspector is taking the session regardless.
          }
        }
      }
      wc.openDevTools({ mode: "detach", activate: true });
    } else {
      wc.closeDevTools();
    }
    // `devtools-opened` / `devtools-closed` push the authoritative state; this is
    // the same push one round trip earlier, so the button does not wait on an
    // event to stop looking un-pressed.
    pushState(window, args.viewId, entry);
  });

  ipcMain.handle("veld:browser:visible", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    setVisible(found.entry, args?.visible === true);
  });

  ipcMain.handle("veld:browser:navigate", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const url = safeUrl(args?.url);
    if (!url) throw new Error("only http and https URLs can be opened in a pane");
    void found.entry.view.webContents.loadURL(url).catch(() => {
      // `did-fail-load` already reported it to the pane; the rejected promise
      // here would just be an unhandled duplicate.
    });
  });

  ipcMain.handle("veld:browser:command", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const wc = found.entry.view.webContents;
    // A guest that closed itself leaves the entry holding a dead WebContents, and
    // every call below throws on one. The renderer has already patched the pane
    // optimistically by this point (`loading: true, error: null`), so a throw here
    // strands it on a spinner with the real error wiped — see `stateOf`, which
    // guards for the same reason.
    if (wc.isDestroyed()) return;
    switch (args?.command) {
      case "back":
        wc.navigationHistory.goBack();
        break;
      case "forward":
        wc.navigationHistory.goForward();
        break;
      case "reload":
        wc.reload();
        break;
      case "stop":
        wc.stop();
        break;
      case "focus":
        wc.focus();
        break;
      default:
        break;
    }
  });

  /**
   * A still of what the view is showing right now, as a JPEG data URL.
   *
   * This is what makes hiding a view survivable: a native view paints over every
   * DOM overlay, so a menu means the view has to go — and a pane that blanks
   * whenever you open a menu reads as broken. The renderer paints this still in
   * its place, so the pane freezes instead of disappearing.
   *
   * Must be called while the view is still visible: capturing a hidden one
   * returns an empty image. JPEG rather than PNG because this crosses IPC on
   * every menu open and a full-pane PNG is an order of magnitude bigger.
   */
  ipcMain.handle("veld:browser:capture", async (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found || found.entry.view.webContents.isDestroyed()) return null;
    if (!found.entry.visible) return null;
    try {
      const image = await found.entry.view.webContents.capturePage();
      if (image.isEmpty()) return null;
      return `data:image/jpeg;base64,${image.toJPEG(72).toString("base64")}`;
    } catch {
      // A view mid-navigation or mid-teardown can refuse; the pane just blanks,
      // which is the behaviour this exists to improve, not to guarantee.
      return null;
    }
  });

  ipcMain.handle("veld:browser:destroy", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    disposeEntry(found.window, found.entry);
    byWindow.get(found.window.id)?.delete(args.viewId);
  });

  /**
   * Clear a session's cookies and storage — "sign out of this session".
   *
   * Addressed by profile, not by view, so a session with no pane open right now
   * can still be cleared: that is what makes the session list manageable rather
   * than a set of names you can only ever add to. The partition is resolved here
   * from a validated slot name, never from a string the page composed.
   *
   * A profile is one partition, so this affects every pane using it. All of them
   * are reloaded, not just the one that asked: leaving a sibling pane rendering a
   * logged-in page whose session no longer exists is a lie about the state.
   */
  ipcMain.handle("veld:browser:clear-session", async (event, args) => {
    const window = senderWindow(event);
    if (!window) return;
    const profile = typeof args?.profile === "string" ? args.profile : "";
    if (!isProfileName(profile)) throw new Error("invalid profile name");
    await session.fromPartition(partitionFor(profile)).clearStorageData();
    for (const entry of byWindow.get(window.id)?.values() ?? []) {
      if (entry.profile !== profile) continue;
      if (!entry.view.webContents.isDestroyed()) entry.view.webContents.reload();
    }
  });
}

/**
 * Show or hide a view, keeping its process and its page.
 *
 * Hiding matters more than it sounds: a native view paints over every DOM
 * overlay, so this is also how the renderer gets a menu or a dialog that
 * overlaps a pane to be visible at all.
 */
function setVisible(entry, visible) {
  entry.visible = visible;
  entry.view.setVisible(visible);
}

module.exports = { registerBrowserViewIpc, disposeWindow };
