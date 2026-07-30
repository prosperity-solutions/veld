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
const { isProfileName, isViewId, partitionFor, safeUrl } = require("./validate");

/**
 * Per-window view cap.
 *
 * Each view is a full renderer process with its own cookie jar; a page that
 * loops `create` would otherwise be a memory bomb. High enough that no real
 * layout hits it.
 */
const MAX_VIEWS_PER_WINDOW = 16;

/** @typedef {{view: import('electron').WebContentsView, profile: string, visible: boolean}} Entry */

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
    ...(error === undefined ? {} : { error }),
  };
}

function send(window, channel, payload) {
  if (window.isDestroyed() || window.webContents.isDestroyed()) return;
  window.webContents.send(channel, payload);
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
  try {
    window.contentView.removeChildView(entry.view);
  } catch {
    // Window already tearing down — nothing to detach from.
  }
  if (!entry.view.webContents.isDestroyed()) entry.view.webContents.close();
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
    const entry = { view, profile, visible: true };
    entries.set(viewId, entry);
    window.contentView.addChildView(view);
    attachListeners(window, viewId, entry);

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
