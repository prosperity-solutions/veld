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

const fs = require("node:fs");
const path = require("node:path");
const {
  BrowserWindow,
  WebContentsView,
  ipcMain,
  screen,
  session,
  webContents: allWebContents,
} = require("electron");
// The trust boundary lives in its own tested module — see src/validate.js.
const {
  isProfileName,
  isViewId,
  safeColor,
  partitionFor,
  isPermissionRule,
  safeEmulation,
  safeMedia,
  safeRadius,
  safeScale,
  safeUrl,
  safeZoom,
} = require("./validate");
// The CSS-pixel → DIP arithmetic every native view's geometry goes through, in the
// same Electron-free module as the top bar's own zoom maths, and tested there.
const { cssBoxToDip, emulationScale, zoomFactor } = require("./windowState");
// The permission policy is likewise its own tested, Electron-free module.
const permissions = require("./permissions");

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
 *            touch: boolean, userAgent: string|null}} Emulation
 */

/**
 * @typedef {{view: import('electron').WebContentsView, profile: string, visible: boolean,
 *            emulation: Emulation|null, zoom: number, defaultUserAgent: string,
 *            media: Record<string, string>|null, scale: number, radius: number,
 *            touchActive: boolean, mediaActive: boolean,
 *            frameReady: boolean, emulated: boolean, dragging: boolean,
 *            touchQueue: Promise<void>|undefined}} Entry
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
      touchActive: false,
      mediaActive: false,
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
    // The three things the pane cannot work out for itself: whether touch and the
    // emulated media features are actually in force (both ride one debugger
    // session, and something else can hold it — see [`applyCdpNow`]) and whether
    // the inspector is open. The emulated size, the
    // zoom and the scale are all the renderer's own. Flat primitives on purpose:
    // the renderer's `patch` compares values with `!==`, so a nested object would
    // count as changed on every event and re-render every pane.
    touchActive: entry.touchActive,
    mediaActive: entry.mediaActive,
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
// Four rules hold this together, each of which is a bug if broken:
//
// 1. **The state lives in the renderer's layout, not here.** Emulation, zoom and
//    UA are per-`WebContents`, and a pane switching session destroys and
//    recreates its view — so everything below is re-asserted from the `create`
//    payload, exactly as the URL is. This module is the applier, not the owner.
// 2. **`enableDeviceEmulation` and `disableDeviceEmulation` need a committed
//    frame.** Calling either on a `WebContentsView` that has not navigated yet
//    **segfaults the whole app** — not an exception, a `SIGSEGV`, so there is
//    nothing to catch and the window is simply gone. Verified on Electron 43,
//    including the `disable` call on a view that never had emulation enabled,
//    which is what the no-device path used to do on every pane. Hence
//    `frameReady`: the metrics are applied on the first `did-navigate` and
//    immediately after that. `setUserAgent` and `setZoomFactor` are safe before a
//    load (checked the same way), which is what matters — the UA has to be set
//    before the first request or the page has to be reloaded to see it.
// 3. **Zoom is re-asserted after every navigation.** Chromium's zoom policy is
//    per *origin*, so navigating adopts whatever that origin was last viewed at,
//    in *any* pane sharing the session. Without re-assertion a pane's zoom
//    silently changes when you follow a link, and setting one pane's zoom moves
//    its neighbour's.
// 4. **Touch is the one thing that can be taken away.** It needs a CDP session
//    (`Emulation.setEmitTouchEventsForMouse` has no Electron API). Electron's docs
//    say opening DevTools detaches an attached debugger; on Electron 43 it does
//    not (the two sessions coexist — measured, not assumed). Both worlds are
//    handled rather than either being trusted: nothing is detached pre-emptively,
//    a `detach` from any cause flips `touchActive` to false, and `devtools-closed`
//    re-attaches. `touchActive` is therefore reported separately from the `touch`
//    the pane asked for, and the pane shows what it actually has.

/**
 * Push the emulated viewport onto the view.
 *
 * `entry.scale` arrives **with the bounds**, from the renderer: the screen's box,
 * the factor its viewport is rendered at and the corner radius are one calculation
 * (`deviceLayout` in `panes/devices.ts`), and the renderer is the side that knows
 * the pane's padding and where the screen is centred. Deriving the factor here
 * from the box meant one number with two owners, which is a drift waiting to be a
 * half-off-screen device.
 *
 * **The factor applied is not the factor received.** The renderer's number is in
 * CSS pixels and the view it renders into is sized in DIP, so `emulationScale`
 * folds in the /ide page's own zoom — the same conversion `cssBoxToDip` does for
 * the box. See that function for what the missing factor looked like.
 */
function applyMetrics(entry) {
  const wc = entry.view.webContents;
  if (wc.isDestroyed()) return;
  const emulation = entry.emulation;
  // Rule 2: both calls below segfault the process on a view with no committed
  // frame. A pane that has not been pointed anywhere yet is exactly that state —
  // it shows the run's URL list, and its view has never navigated — so the
  // emulation waits for the first navigation, which re-applies it (see
  // `attachListeners`). `emulated` keeps the no-device path from calling `disable`
  // on a view that never had it on, which is a call with nothing to undo.
  if (!entry.frameReady) return;
  if (!emulation) {
    if (!entry.emulated) return;
    entry.emulated = false;
    entry.metricsScale = null;
    wc.disableDeviceEmulation();
    return;
  }
  entry.emulated = true;
  // Recorded because `enableDeviceEmulation` relayouts the guest page, so every
  // caller has to be able to ask "would this change anything" first — and the
  // question is about the *applied* factor, which moves when the page zooms even
  // though the renderer's number has not.
  const scale = emulationScale(entry.scale, entry.hostZoom);
  entry.metricsScale = scale;
  wc.enableDeviceEmulation({
    // `mobile` is Chromium's own word for viewport-meta handling, overlay
    // scrollbars and text autosizing — not for touch, which is separate below.
    screenPosition: emulation.mobile ? "mobile" : "desktop",
    screenSize: { width: emulation.width, height: emulation.height },
    viewSize: { width: emulation.width, height: emulation.height },
    viewPosition: { x: 0, y: 0 },
    deviceScaleFactor: emulation.deviceScaleFactor,
    scale,
  });
}

/**
 * Clip the page to the emulated screen's corners.
 *
 * `entry.radius` is the renderer's number, already scaled to the size the screen
 * is *drawn* at (`scaledRadius` in `panes/devices.ts`) — but in CSS pixels, like
 * the box, so it needs the same zoom factor. `setBorderRadius` clips the page; the
 * frame the renderer draws around it is what makes the corners visible, since a
 * native view paints over any DOM inside its own rect.
 */
function applyRadius(entry) {
  const radius = Math.round(entry.radius * zoomFactor(entry.hostZoom));
  if (radius === entry.appliedRadius) return;
  entry.appliedRadius = radius;
  entry.view.setBorderRadius(radius);
}

/**
 * Re-place every one of this window's views for a new page zoom factor.
 *
 * **Called from the poll that already follows the factor** (`syncZoom` in
 * `windows.js`), rather than waiting for the page to re-measure its panes. A zoom
 * step does reflow the page and normally re-pushes every box — but "normally" is
 * a side effect, not a contract: a device that exactly fills its pane can round to
 * the same integer rect, and then a zoomed /ide leaves its native views over the
 * wrong region with nothing to correct it. Electron has no event covering every
 * way the factor can change (see `ZOOM_POLL_MS`), which is why this is pushed here
 * rather than derived per bounds call.
 */
function syncWindowZoom(window, zoom) {
  const entries = byWindow.get(window.id);
  if (!entries) return;
  const factor = zoomFactor(zoom);
  for (const entry of entries.values()) {
    if (entry.hostZoom === factor) continue;
    entry.hostZoom = factor;
    if (entry.view.webContents.isDestroyed()) continue;
    // The last box the renderer sent, converted again. A view that has never been
    // given one has nothing to re-place — its first bounds push will use the new
    // factor anyway.
    if (entry.cssRect) {
      const rect = cssBoxToDip(entry.cssRect, factor);
      if (rect.width >= 1 && rect.height >= 1) entry.view.setBounds(rect);
    }
    applyRadius(entry);
    if (entry.emulation && emulationScale(entry.scale, factor) !== entry.metricsScale) {
      applyMetrics(entry);
    }
  }
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
 *
 * **Serialised per view.** Every branch here awaits a CDP round trip, so two runs
 * can interleave — and toggling the menu's Touch item twice inside one round trip
 * does exactly that, since the item deliberately keeps the menu open. An off-run
 * resuming after an on-run detached the debugger while `emulation.touch` was still
 * true, leaving the pane claiming touch was paused by something else, which was a
 * lie with nothing to retry it until DevTools closed or the page navigated. Chaining
 * makes the last caller win, which is what a toggle means.
 */
function applyTouch(window, viewId, entry) {
  entry.touchQueue = (entry.touchQueue ?? Promise.resolve())
    .then(() => applyCdpNow(window, viewId, entry))
    .catch(() => {});
  return entry.touchQueue;
}

/**
 * The media features a pane can override, and the CDP value for each.
 *
 * `Emulation.setEmulatedMedia` is one call for all of them: the `features` array
 * replaces the whole set, so an empty array is the reset and there is no
 * per-feature clear to get wrong.
 */
const MEDIA_FEATURES = ["prefers-color-scheme", "prefers-reduced-motion", "forced-colors"];

/** Whether anything is being overridden — `null` per feature means "the host's". */
function hasMediaOverrides(media) {
  return MEDIA_FEATURES.some((name) => media?.[name]);
}

/**
 * The body of [`applyTouch`], which now owns **everything CDP**.
 *
 * One function for touch and media because they share one debugger session and
 * the earlier split would have broken both: turning touch off detached the
 * session, which silently dropped a media override that was still meant to be in
 * force. So the attach is driven by whether *anything* wants it, and the detach
 * only happens when nothing does.
 *
 * **Never call this directly** — it awaits CDP round trips, and two interleaved
 * runs are exactly what the queue exists to prevent.
 */
async function applyCdpNow(window, viewId, entry) {
  const wc = entry.view.webContents;
  if (wc.isDestroyed()) return;
  const wantTouch = entry.emulation?.touch === true;
  const wantMedia = hasMediaOverrides(entry.media);
  const dbg = wc.debugger;

  const report = (touch, media) => {
    if (entry.touchActive === touch && entry.mediaActive === media) return;
    entry.touchActive = touch;
    entry.mediaActive = media;
    pushState(window, viewId, entry);
  };

  if (!wantTouch && !wantMedia) {
    if ((entry.touchActive || entry.mediaActive) && dbg.isAttached()) {
      // Explicitly off before detaching. Detaching *should* drop the session's
      // overrides on its own, but "should" is the wrong footing for a page that
      // would otherwise keep answering mouse drags with touch events.
      try {
        await dbg.sendCommand("Emulation.setEmitTouchEventsForMouse", { enabled: false });
        await dbg.sendCommand("Emulation.setTouchEmulationEnabled", { enabled: false });
        await dbg.sendCommand("Emulation.setEmulatedMedia", { features: [] });
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
    report(false, false);
    return;
  }

  try {
    if (!dbg.isAttached()) dbg.attach("1.3");
  } catch (error) {
    // Something else holds Chromium's debugger for this view — the built-in
    // DevTools is the usual one. Reported as suspended rather than as on, and
    // retried when DevTools closes.
    console.warn("[veld] CDP attach refused", error);
    report(false, false);
    return;
  }

  try {
    // Both are sent every run, including the "off" side: with the session shared,
    // a pane that turns touch off while a media override keeps the debugger
    // attached still has to be *told* touch is off.
    //
    // **`maxTouchPoints` only when enabling.** CDP validates it to 1–16, so
    // sending `0` alongside `enabled: false` is a protocol error — which threw
    // before `setEmulatedMedia` was reached and reported the media override as
    // suspended. It was invisible while touch and media shared no code, because
    // the old off-path sent `{ enabled: false }` and nothing else; merging the
    // two features onto one applier is what introduced it.
    await dbg.sendCommand(
      "Emulation.setTouchEmulationEnabled",
      wantTouch ? { enabled: true, maxTouchPoints: 1 } : { enabled: false },
    );
    await dbg.sendCommand(
      "Emulation.setEmitTouchEventsForMouse",
      wantTouch ? { enabled: true, configuration: "mobile" } : { enabled: false },
    );
    await dbg.sendCommand("Emulation.setEmulatedMedia", {
      features: MEDIA_FEATURES.filter((name) => entry.media?.[name]).map((name) => ({
        name,
        value: entry.media[name],
      })),
    });
    report(wantTouch, wantMedia);
  } catch (error) {
    // A command was refused, or the view is tearing down. The pane shows both as
    // suspended rather than as on — the same honesty `touchActive` has always
    // had, now for the feature that shares its session.
    //
    // Logged as well as reported: "the emulated colour scheme did nothing" and
    // "the CDP call threw" look identical from the pane, and only one of them is
    // veld's bug. This log is what identified exactly that.
    console.warn("[veld] CDP emulation command failed", error);
    report(false, false);
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
 * Apply everything an emulation controls.
 *
 * Split by *when it is safe*, not by what it does: the user agent is set before
 * the first `loadURL` — that is the whole point of it, since a document reads
 * `navigator.userAgent` once, while it loads — but the metrics need a committed
 * frame or the process dies (rule 2), so on a fresh view they land on the first
 * `did-navigate` instead. `applyMetrics` enforces that itself, so this is safe to
 * call at any point in a view's life.
 */
function applyEmulation(window, viewId, entry) {
  applyUserAgent(entry);
  applyMetrics(entry);
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

  // The per-site panel is per *site*, so it has to be re-sent when the site
  // changes. Not on `did-navigate-in-page`: a fragment change cannot cross an
  // origin, and re-sending on every `pushState` would repaint the panel while a
  // single-page app routes.
  wc.on("did-navigate", () => pushPermissionState(window, viewId, entry));

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

  // Chromium's own answer to `findInPage`/`stopFindInPage` below — the match
  // count and which one is active. This always reflects the live page: a
  // suspended pane's frozen still is only ever a paint substitute (`applyVisibility`
  // in the renderer), never a substitute source for what gets searched.
  //
  // One request can raise several of these as Chromium scopes a long page, and
  // only the one with `finalUpdate` is the authoritative tally — an earlier one
  // can under-report (a fresh search reporting 0 matches before it has scanned
  // far enough), which the pane would otherwise show as "No results" for a
  // query that has some, for a moment. Forwarding only the final one is what a
  // real production find-bar implementation does (electron-in-page-search
  // gates on the same field before emitting).
  wc.on("found-in-page", (_e, result) => {
    if (!result.finalUpdate) return;
    send(window, "veld:browser:find-result", {
      viewId,
      requestId: result.requestId,
      matches: result.matches,
      activeMatchOrdinal: result.activeMatchOrdinal,
    });
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
  // Bindings intercepted and forwarded: `Ctrl/⌘+Shift+P` for the command
  // palette, and `Ctrl/⌘+F` for the pane's own find bar — both app-documented
  // shortcuts. `⇧P` is safely outside anything a previewed page wants for
  // itself; `F` is a real, accepted trade-off rather than a free one — a
  // dev-server preview with its own find (a docs site, an embedded
  // Monaco/CodeMirror editor) loses that binding entirely, since this fires
  // ahead of the page's own key handlers and there is no escape hatch. This
  // is the same trade a real browser tab already makes: Chrome's own find bar
  // owns `Ctrl/⌘+F` unconditionally too, so a page cannot claim it there
  // either — this pane behaving the same way is consistent with that, not a
  // new risk this diff introduces. No other window-level shortcut is forwarded
  // from here, and the reasons differ. **Tab cycling is a settled decision, not
  // a candidate** — see the note in the handler below; forwarding it takes a
  // text-selection chord from every guest page, which is the cost `⌘F` is
  // allowed only because it hands back an equivalent. The rest (worktree
  // navigation, focus mode, the view switch, update main, run cycling,
  // start/stop, restart, opening the Shortcuts overview) are open follow-ups
  // rather than oversights; clicking back into the rail or top bar reaches them
  // same as before this pane existed.
  //
  // `⌘T`/`⌘W`/`⌘⇧W` need nothing here at all: they are *menu* accelerators
  // (`main.js`), which are handled before web contents see the key.
  //
  // (That preamble documents `before-input-event`, two statements below — not
  // the focus listener immediately after this line.)

  // This pane's page took the keyboard.
  //
  // A `WebContentsView` is an OS-level widget outside the host document, so
  // clicking into its page fires no `focusin` in `/ide` and the layout's
  // `focused` dock silently keeps whatever it was — which was harmless while
  // nothing destructive read it, and stopped being harmless when ⌘W started
  // closing "the focused dock's active tab". Clicking a browser pane in the
  // right dock and pressing ⌘W would close a terminal in the left one, with no
  // confirmation if that shell happened to be idle.
  //
  // Reported rather than inferred: only this process can see the native focus,
  // and the renderer resolves the view id back to the dock its tab sits in.
  wc.on("focus", () => send(window, "veld:browser:focused", { viewId }));

  wc.on("before-input-event", (event, input) => {
    if (input.type !== "keyDown") return;
    if (
      (input.control || input.meta) &&
      !input.shift &&
      !input.alt &&
      input.key.toLowerCase() === "f"
    ) {
      event.preventDefault();
      // Move the keyboard back to the page first: forwarding the accelerator
      // alone would open the bar while every keystroke still went to the view,
      // so the input would be there and unusable.
      window.webContents.focus();
      send(window, "veld:browser:accelerator", { viewId, accelerator: "find" });
      return;
    }
    // **The navigation family, forwarded**: `⌃Tab`/`⌃⇧Tab` steps tabs and
    // `⌥Tab`/`⌥⇧Tab` steps worktrees. This is the only way they reach a focused
    // pane — a `Tab` menu accelerator does not work, because Chromium's focus
    // manager handles `Tab` before the menu layer and the page tabs anyway.
    //
    // **What this costs the guest page, stated accurately.** An earlier draft
    // of this comment claimed `⌃Tab` is browser-reserved so a page never sees
    // it — that is false here. A `WebContentsView` has no tab strip, so Ctrl+Tab
    // *is* delivered to the guest renderer, and a web IDE in a pane
    // (code-server, VS Code for the Web, Jupyter) really does lose its
    // editor-tab chord. `⌥Tab` is the cheap half: it only duplicates plain
    // `Tab`'s focus traversal, which the page keeps.
    //
    // Taken anyway, deliberately: navigating *out of* a pane is the case this
    // whole family exists for, and a page-dispatched chord cannot reach a
    // focused pane at all. It is still a smaller loss than `⌘⇧`+arrow, which was
    // forwarded, shipped and reverted for eating live text selection in *every*
    // page rather than an editor chord in a rare one — but it is a loss, not a
    // free lunch, and the next person weighing a forwarded chord should read it
    // that way.
    if (input.key === "Tab" && !input.meta) {
      const ctrlOnly = input.control && !input.alt;
      const altOnly = input.alt && !input.control;
      if (ctrlOnly || altOnly) {
        event.preventDefault();
        // The keyboard goes back to the page, as it does for ⌘F above: the pane
        // being navigated away from must not keep the next keystroke.
        window.webContents.focus();
        send(window, "veld:browser:accelerator", {
          viewId,
          accelerator: `${ctrlOnly ? "tab" : "worktree"}:${input.shift ? "previous" : "next"}`,
        });
        return;
      }
    }
    // Splitting the dock (`⌘⇧D`), and worktrees off macOS (`Ctrl+Shift+B`/`N`).
    // Same trade as the Tab chords: nothing a guest page can meaningfully use.
    if ((input.control || input.meta) && input.shift && !input.alt) {
      if ((input.key || "").toLowerCase() === "d") {
        event.preventDefault();
        window.webContents.focus();
        send(window, "veld:browser:accelerator", { viewId, accelerator: "split" });
        return;
      }
    }
    if (input.control && input.shift && !input.alt && !input.meta) {
      const letter = (input.key || "").toLowerCase();
      if (letter === "b" || letter === "n") {
        event.preventDefault();
        window.webContents.focus();
        send(window, "veld:browser:accelerator", {
          viewId,
          accelerator: `worktree:${letter === "b" ? "previous" : "next"}`,
        });
        return;
      }
    }
    // Project switching: `Ctrl/⌘+1`…`9` and `Ctrl/⌘+\``. Forwarded rather than made
    // menu accelerators because the main process does not know the project list —
    // a menu would have to carry nine items labelled "Project 3" or be fed the
    // names over a channel of their own, for a binding the page can act on
    // directly. Unlike `F` above this takes nothing from the previewed page: a
    // `Ctrl/⌘`+digit is a browser-level binding everywhere (tab switching), so no
    // page has it to lose.
    //
    // `input.code`, not `input.key`: on AZERTY and several other layouts the
    // unshifted digit row is punctuation, and ⌘2 means the key with 2 printed on
    // it. `key` is kept as the fallback for a layout with no `code`.
    //
    // `1-9` here is a **coarse pre-filter, not the authority** — the renderer bounds
    // the digit against `MAX_PROJECT_SHORTCUTS` (`shared/projects.ts`) before it
    // addresses anything, so erring wide on this side costs a forwarded message that
    // resolves to nothing. Keep it wide rather than mirroring a constant this process
    // cannot import.
    if ((input.control || input.meta) && !input.shift && !input.alt) {
      const digit = /^Digit([1-9])$/.exec(input.code || "")?.[1] ?? null;
      const isDigit = digit ?? (/^[1-9]$/.test(input.key) ? input.key : null);
      // **The keyboard moves to the page, as it does for ⌘F above and ⌘⇧P below.**
      // A switch replaces what is on screen — the pane the user was typing in
      // belongs to the worktree they are leaving — so leaving the keyboard in a
      // native view that is about to be hidden means typing into nothing.
      //
      // The accepted cost, stated because it was briefly "fixed" the wrong way: a
      // chord that resolves to *no* switch (one project, or a digit past the last)
      // still takes focus out of the pane. The main process cannot tell the two
      // apart — it does not know the project list — and losing the keyboard on a
      // real switch is the worse of the two, being also the common one.
      if (isDigit) {
        event.preventDefault();
        window.webContents.focus();
        send(window, "veld:browser:accelerator", {
          viewId,
          accelerator: `project:${isDigit}`,
        });
        return;
      }
      if (input.key === "`" || input.code === "Backquote") {
        event.preventDefault();
        window.webContents.focus();
        send(window, "veld:browser:accelerator", { viewId, accelerator: "project:toggle" });
        return;
      }
    }
    if (!input.shift) return;
    if (!(input.control || input.meta)) return;
    if (input.key.toLowerCase() !== "p") return;
    event.preventDefault();
    window.webContents.focus();
    send(window, "veld:browser:accelerator", { viewId, accelerator: "palette" });
  });

  // While the pane's screen is being resized by dragging its edge, forward the
  // pointer to the page.
  //
  // This is what lets the *real* view resize live instead of being hidden for the
  // gesture. A `WebContentsView` is an OS-level widget with its own WebContents, so
  // a mouse event over it belongs to the guest and the /ide document never sees it —
  // a drag whose pointer crosses the view therefore loses both its moves and its
  // `mouseup`, leaving a gesture that can never end. Forwarding closes that hole at
  // the source instead of with a timeout heuristic (and a heuristic would have to
  // guess between "the pointer is over the page" and "the user is holding still").
  //
  // The **position comes from the cursor, not from the event**: `input-event`'s
  // `x`/`y` are documented without saying which space they are in, and a
  // half-window offset in a drag is not a bug worth shipping to discover.
  // `getCursorScreenPoint` minus the window's content origin is unambiguous, and
  // dividing by the zoom factor is the exact inverse of the CSS→DIP conversion the
  // bounds handler does — so the page receives the coordinates its own
  // `pointermove` would have carried.
  wc.on("input-event", (_e, input) => {
    if (!entry.dragging) return;
    if (input.type !== "mouseMove" && input.type !== "mouseUp") return;
    if (window.isDestroyed() || window.webContents.isDestroyed()) return;
    const cursor = screen.getCursorScreenPoint();
    const content = window.getContentBounds();
    const zoom = window.webContents.getZoomFactor() || 1;
    send(window, "veld:browser:pointer", {
      viewId,
      type: input.type,
      x: (cursor.x - content.x) / zoom,
      y: (cursor.y - content.y) / zoom,
    });
  });

  // The first committed navigation is what makes the emulation calls safe to make
  // at all (rule 2), and every later one is what keeps zoom and touch in force.
  //
  // Zoom, because Chromium's zoom policy is **per origin**: a navigation adopts
  // whatever the destination origin was last viewed at — including a level set by
  // a *different* pane on the same session — so a pane that does not re-assert
  // changes zoom on its own as you browse. Touch, because the CDP overrides are
  // tied to the session and a cross-origin navigation can replace the render frame
  // underneath them. Re-sending both is cheap and idempotent: `applyMetrics`
  // recomputes the same scale, and `applyTouch` only pushes state when the answer
  // changes.
  const ready = () => {
    const first = !entry.frameReady;
    entry.frameReady = true;
    applyMetrics(entry);
    applyZoom(entry);
    void applyTouch(window, viewId, entry);
    // The scale is part of what the pane renders, and on the first navigation it
    // has just gone from "asked for" to "in force".
    if (first) push();
  };
  // **`did-navigate` only**, deliberately. A `did-fail-load` listener used to open
  // this gate too, on the reasoning that a failed load still commits an error page
  // and setting a device from the pane's error screen has to work. That reasoning is
  // false for the one code that matters: `ERR_ABORTED` (-3) commits *nothing* — it is
  // what Stop on a first load reports, and what a navigation superseded by another
  // one reports — so the gate opened on a view with no frame, and the next
  // `applyMetrics` then made exactly the `enableDeviceEmulation` call rule 2 says
  // takes the whole app down with it. Nothing is lost: a committed error page fires
  // `did-navigate` as well, which is the case that listener was added for, and while
  // a load has failed the pane is showing its own error screen with the view hidden
  // anyway (`paneCovers`). Note that this file already knows -3 is not a fault — see
  // the `did-fail-load` handler above, which filters it for the same reason.
  wc.on("did-navigate", ready);
  // A dead renderer has no frame to emulate against, which is the state rule 2 is
  // about. The next navigation makes it safe again.
  wc.on("render-process-gone", () => {
    entry.frameReady = false;
    entry.emulated = false;
  });

  // Electron's docs say opening DevTools detaches an attached debugger; on
  // Electron 43 the two coexist. Nothing is detached pre-emptively on that basis —
  // doing so threw touch away for a conflict this version does not have — so this
  // reacts instead: a `detach` from any cause is reported as touch being off, and
  // closing DevTools retries the attach in case that version does take it.
  wc.on("devtools-opened", () => push());
  wc.on("devtools-closed", () => {
    push();
    void applyTouch(window, viewId, entry);
  });
  wc.debugger.on("detach", () => {
    // Both, because both ride this one session: a detach from any cause takes the
    // media override with the touch emulation.
    if (!entry.touchActive && !entry.mediaActive) return;
    entry.touchActive = false;
    entry.mediaActive = false;
    push();
  });

  wirePermissionHandlers(wc.session, partitionFor(entry.profile));
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------
//
// Panes used to refuse every permission outright. What replaces that is
// `permissions.js` — a policy over the user's stored answers, the project's
// `ide.permissions`, and veld's defaults — plus the two things only this file can
// do: raise the prompt, and answer Electron.
//
// Three constraints shape the code below.
//
// **Handlers are per *session*, not per view.** Panes sharing a profile share one
// `Session`, so registering in `attachListeners` would re-register the same
// handler for every pane in that jar, and — worse — the handler is asked about
// *any* WebContents on the session, including a pane's detached DevTools
// frontend. So the registration is per partition and the dispatch starts by
// resolving the WebContents back to a pane. Anything unresolvable is denied:
// there is no pane to attribute it to and no prompt that could name one.
//
// **The check handler is synchronous.** It cannot prompt, so "ask" has to answer
// `false` there — a page's `navigator.permissions.query()` therefore reports
// `denied` until a real request has been answered. That is the one place the
// policy is visibly poorer than a browser's, and it is Electron's API shape, not
// a choice. It is also the strongest argument for `ide.permissions`: a config
// answer *is* available synchronously.
//
// **A prompt's answer is sticky**, and for screen capture that is deliberately
// *stickier than Chrome*. Allow and Block are remembered for (session, origin,
// permission) rather than being per-invocation — which is what lets
// `setDisplayMediaRequestHandler`, running *after* the `display-capture` request
// has already been answered, re-resolve the same verdict instead of raising a
// second prompt for one `getDisplayMedia` call.
//
// Chrome persists camera and microphone but pointedly does **not** persist
// display capture: one Allow there covers one capture. Veld's answer to the gap
// that opens is the user-gesture requirement in the display-media handler — a
// remembered Allow still cannot be cashed in by a script on page load — plus the
// per-site panel, where the grant is visible and revocable. A per-invocation
// prompt would be the stricter design; it needs a one-shot token threaded
// through the second pass, which is a bigger change than this batch.

/** Where user answers are persisted. Set by [`registerBrowserViewIpc`]. */
let permissionsFile = null;

/** partition → origin → permission id → "allow" | "deny". */
let permissionStore = {};

/**
 * Answers this process has deliberately **removed**, which a merge cannot infer.
 *
 * [`persistPermissions`] merges with the file rather than overwriting it, so that
 * a second app instance does not clobber this one's answers. The cost of merging
 * is that an *absence* carries no information: "this process never saw it" and
 * "this process deleted it" look identical, and the merge resolves both in favour
 * of the file. That is fail-**open** — pressing Default on a granted permission,
 * or clearing a session, would be undone by the very next write.
 *
 * So deletions are recorded rather than inferred. `revoked` holds
 * `partition\0origin\0id` for a single permission set back to Default and is
 * undone by a later answer for that key. `clearedPartitions` holds a whole
 * session that was signed out and is **never** undone: it means "ignore the
 * file's pre-clear contents for this partition", so re-granting inside a cleared
 * session works without removing the marker — which matters because
 * `persistPermissions` swallows write failures, and a marker dropped before its
 * write landed would let every signed-out grant merge back.
 */
const revoked = new Set();
const clearedPartitions = new Set();

/**
 * Record one answer, and keep the deletion bookkeeping straight.
 *
 * The only writer of `permissionStore`, so the two sets above cannot drift from
 * it per call site. Setting a permission back to Default *is* a deletion and has
 * to be remembered as one; setting it to Allow or Block undoes both a previous
 * revocation and a session clear, or the panel could not re-grant something in
 * the session it had just signed out of.
 */
function recordAnswer(partition, origin, id, verdict) {
  const key = permissions.originKey(origin);
  if (!key) return;
  permissionStore = permissions.setAnswer(permissionStore, partition, origin, id, verdict);
  if (verdict === "allow" || verdict === "deny") {
    revoked.delete(permissions.revocationKey(partition, key, id));
    // `clearedPartitions` is deliberately *not* cleared here. It no longer means
    // "delete this partition" — `mergeForWrite` reads it as "ignore the file's
    // pre-clear contents for it" — so a later answer is preserved on its own
    // merits, and the marker can safely outlive a write that failed.
  } else {
    revoked.add(permissions.revocationKey(partition, key, id));
  }
}

/** Partitions whose session already carries our handlers. */
const wiredPartitions = new Set();

/**
 * window.id → the policy inputs its renderer last pushed.
 *
 * Per window because a window shows one worktree, and the rules come from *that*
 * checkout's config. `trustedOrigins` are the URLs veld itself serves for the
 * selected run — the only origins screen capture is granted at without asking.
 */
/** @type {Map<number, {rules: unknown[], trustedOrigins: string[]}>} */
const policyByWindow = new Map();

/**
 * How long a prompt may go unanswered before it is released.
 *
 * Not a UX timeout — a stuck-request backstop for the one case the renderer
 * cannot cover, where a prompt is raised for a pane whose chrome was never
 * mounted and nothing is subscribed to receive it. Generous, because a prompt
 * somebody is actually reading must never expire under them.
 */
const PROMPT_TIMEOUT_MS = 5 * 60 * 1000;

/**
 * requestId → the prompt awaiting an answer from a renderer.
 *
 * The window id travels with it so a closing window can settle exactly its own
 * prompts — the page behind a prompt nobody can answer any more is otherwise
 * waiting on a callback that will never fire. The view id does the same for a
 * closing *tab*, and `expiry` is the backstop timer above, cleared by
 * `releasePrompt` whichever way the prompt ends.
 */
/** @type {Map<number, {windowId: number, viewId: string, expiry: NodeJS.Timeout,
 *                      resolve: (verdict: string|null) => void}>} */
const pendingPrompts = new Map();
let nextPromptId = 1;

function loadPermissions() {
  if (!permissionsFile) return {};
  try {
    return permissions.sanitizeStore(JSON.parse(fs.readFileSync(permissionsFile, "utf8")));
  } catch {
    // Missing on first run, and unreadable or corrupt is the same answer: start
    // from nothing granted. `sanitizeStore` makes the same choice field by field.
    return {};
  }
}

/**
 * Merge this process's answers into whatever is on disk, then write.
 *
 * **Read-merge-write, not write.** `main.js` lets an unpackaged instance run
 * beside the packaged one, and `app.setName("Veld")` gives both the same
 * `userData` — so two processes target this file. A wholesale write from an
 * in-memory copy read at startup means the last one to quit silently discards the
 * other's answers, and the loss is **fail-open** in the case that matters: a user
 * Block outranks a config `allow`, so losing it resumes a grant they had
 * explicitly refused.
 *
 * This process wins per (partition, origin, permission) — it holds the answer the
 * user just gave — while anything it has never seen is preserved. Not atomic
 * against a concurrent writer, and does not pretend to be: the window is one
 * file read, and the failure it removes is the common one (two instances used
 * hours apart) rather than the rare one.
 */
function persistPermissions() {
  if (!permissionsFile) return;
  try {
    const merged = permissions.mergeForWrite(loadPermissions(), permissionStore, {
      revoked: [...revoked],
      cleared: [...clearedPartitions],
    });
    fs.mkdirSync(path.dirname(permissionsFile), { recursive: true });
    // Write-then-rename, as `windows.js` does: a torn file parses as "nothing
    // granted", which is safe but silently costs every answer the user gave.
    const tmp = `${permissionsFile}.tmp`;
    fs.writeFileSync(tmp, `${JSON.stringify(merged, null, 2)}\n`);
    fs.renameSync(tmp, permissionsFile);
  } catch {
    // An unwritable userData costs the answers on the next launch and nothing
    // else — this session keeps the in-memory store.
  }
}

/**
 * The pane a WebContents belongs to, or `null`.
 *
 * `null` is the answer for a detached DevTools frontend, a view mid-disposal, and
 * anything else sharing the session — every one of which must be denied rather
 * than inheriting the pane's grants.
 */
function paneOf(wc) {
  if (!wc || wc.isDestroyed()) return null;
  // **By id, not by object identity.** `view.webContents` is a getter, and a
  // WebContents reached another way — `webContents.fromFrame`, or the argument
  // Electron hands a session handler — is not guaranteed to be the same JS
  // wrapper as the one held here. Identity compared equal often enough to look
  // right and failed in exactly the place that matters: a miss here is a silent
  // `callback(false)`, so every permission was denied and none of them prompted,
  // while the per-site panel (which is handed its entry and never comes through
  // this function) went on rendering the correct verdicts.
  const id = wc.id;
  for (const [windowId, entries] of byWindow) {
    for (const [viewId, entry] of entries) {
      const paneWc = entry.view.webContents;
      if (!paneWc.isDestroyed() && paneWc.id === id) {
        return { windowId, viewId, entry };
      }
    }
  }
  return null;
}

/**
 * The origin to attribute a permission request to.
 *
 * Electron's request details are not uniform — `requestingUrl` is documented as
 * absent for a cross-origin subframe, `securityOrigin` appears only on some
 * request shapes, and the check handler passes its origin as a separate argument
 * — so reading one field and denying when it is empty makes an ordinary request
 * unattributable. The pane's own committed URL is the last resort, and it is the
 * same value the per-site panel resolves against, which is what keeps the panel
 * and the prompt from disagreeing about what the site is.
 *
 * **The fallback is main-frame only, and that is the whole safety of it.** An
 * opaque origin — a sandboxed iframe, `srcdoc`, a `data:` document — serialises
 * as the literal string `"null"`, which no parser here accepts. Falling back to
 * the pane's top-level URL for one of those would attribute a *subframe's*
 * request to the embedding page: the frame would inherit whatever that page was
 * granted by `veld.json` or by the user, and the prompt would name the parent
 * while something else did the asking. A subframe veld cannot name gets no
 * origin and is therefore denied, which is the only honest answer.
 */
function requestOrigin(pane, isMainFrame, ...candidates) {
  for (const candidate of candidates) {
    const origin = permissions.parseOrigin(candidate);
    if (origin) return origin;
  }
  if (!isMainFrame) return null;
  const wc = pane.entry.view.webContents;
  return wc.isDestroyed() ? null : permissions.parseOrigin(wc.getURL());
}

function policyFor(windowId) {
  return policyByWindow.get(windowId) ?? { rules: [], trustedOrigins: [] };
}

/**
 * The live window behind an id, or `null`.
 *
 * `byWindow` is keyed by id and holds views, not windows, and a handler that
 * fires while a window is tearing down has an id that no longer resolves.
 */
function windowById(windowId) {
  const window = BrowserWindow.fromId(windowId);
  return window && !window.isDestroyed() ? window : null;
}

/** The inputs `permissions.resolve` needs for a request on one pane. */
function policyInputs(pane, origin) {
  const { rules, trustedOrigins } = policyFor(pane.windowId);
  return {
    origin,
    rules,
    trustedOrigins,
    stored: permissionStore[partitionFor(pane.entry.profile)] ?? {},
  };
}

/**
 * Put the question to the user, and resolve with their answer.
 *
 * The prompt is rendered by the pane's own UI rather than as a native dialog,
 * because the whole reason panes refused permissions before was that a dialog
 * saying "Veld" cannot honestly ask on example.com's behalf. In the pane it can
 * name the origin, the pane and its session colour.
 *
 * Resolves to `"allow"`, `"deny"`, or **`null` when nobody answered** — the
 * window went away, or the prompt was abandoned. The distinction is load-bearing:
 * an unanswered prompt still has to deny *this request*, because a page waiting on
 * a callback that never fires is a hung feature with no error, but it must **not
 * be remembered**. Storing it wrote a permanent Block for that site and
 * permission, outranking the project config, with nothing in the UI explaining why
 * a granted permission had stopped working.
 */
function askUser(window, viewId, entry, ids, origin, details) {
  if (window.isDestroyed() || window.webContents.isDestroyed()) return Promise.resolve(null);
  const requestId = nextPromptId++;
  return new Promise((resolve) => {
    // **A backstop, because one case cannot be fixed from the renderer.** Only the
    // *active* tab renders a pane's chrome, so a request from a background pane —
    // whose page is still running — is sent to a window where nothing is
    // subscribed. Nobody answers, nobody abandons, and the page's promise never
    // settles: a hung feature with no error, which is precisely what this whole
    // surface replaced. The renderer releases what it can see (a second
    // concurrent prompt, a cross-origin navigation, its own unmount); this covers
    // the prompt it never received. Generous, because a visible prompt a person
    // is reading must never expire under them.
    const expiry = setTimeout(() => {
      const pending = pendingPrompts.get(requestId);
      if (!pending) return;
      pendingPrompts.delete(requestId);
      warnDenied(
        ids.join(", "),
        "no answer — the pane's chrome was never shown, or the prompt was left open",
      );
      pending.resolve(null);
    }, PROMPT_TIMEOUT_MS);
    // Never keep the app alive for a prompt nobody is looking at.
    expiry.unref?.();
    pendingPrompts.set(requestId, { windowId: window.id, viewId, resolve, expiry });
    send(window, "veld:browser:permission-request", {
      requestId,
      viewId,
      profile: entry.profile,
      permissions: ids,
      origin: permissions.originKey(origin),
      // A cross-origin subframe asking is a different sentence from the page
      // asking, and the pane is the only surface that can say which it was.
      isMainFrame: details?.isMainFrame !== false,
      paneUrl: entry.view.webContents.isDestroyed() ? "" : entry.view.webContents.getURL(),
    });
  });
}

function settlePrompt(windowId, requestId, verdict) {
  const pending = pendingPrompts.get(requestId);
  // The window check is the same ownership rule the rest of this file applies:
  // one window's renderer must not be able to answer another's prompt.
  if (!pending || pending.windowId !== windowId) return;
  releasePrompt(requestId, pending);
  pending.resolve(verdict === "allow" ? "allow" : "deny");
}

/** Forget a prompt and stop its backstop timer. Never resolves — the caller
 *  decides what the answer was. */
function releasePrompt(requestId, pending) {
  clearTimeout(pending.expiry);
  pendingPrompts.delete(requestId);
}

/**
 * Release prompts nobody can answer any more, so no page waits forever.
 *
 * `null`, not `"deny"`: nobody answered these, and the caller must not record an
 * answer nobody gave.
 *
 * Scoped by window *or* by view, because a prompt outlives its pane in two ways
 * and only one of them was handled: closing the window, and closing (or
 * session-switching) the **tab**, which disposes the view and leaves the prompt's
 * `resolve` closure alive with nothing left that could ever call it.
 */
function abandonPrompts({ windowId, viewId }) {
  for (const [requestId, pending] of pendingPrompts) {
    const mine =
      viewId === undefined ? pending.windowId === windowId : pending.viewId === viewId;
    if (!mine) continue;
    releasePrompt(requestId, pending);
    pending.resolve(null);
  }
}

/**
 * Register the permission handlers on one session, once.
 *
 * Both handlers, not just the request one: Electron's own docs say "you must also
 * implement setPermissionRequestHandler to get complete permission handling. Most
 * web APIs do a permission check and then make a permission request if the check
 * is denied" — and four permissions appear in the *check* union and never in the
 * request one (`deprecated-sync-clipboard-read`, `hid`, `serial`, `usb`). Sync
 * clipboard read is the concrete one: with no check handler a previewed page can
 * `document.execCommand("paste")` and read whatever the user last copied.
 */
function wirePermissionHandlers(ses, partition) {
  if (wiredPartitions.has(partition)) return;
  wiredPartitions.add(partition);

  ses.setPermissionRequestHandler((wc, permission, callback, details) => {
    const pane = paneOf(wc);
    // Reported, not swallowed. Every branch below that denies without asking is
    // indistinguishable, from the page's side, from a policy that said no — which
    // is how a resolution bug reads as "the config does not work". One line on
    // stderr turns the next occurrence into a five-second diagnosis.
    if (!pane) return denyUnattributable(callback, permission, "no pane owns this WebContents");
    const window = windowById(pane.windowId);
    if (!window) return denyUnattributable(callback, permission, "the pane's window is gone");
    const origin = requestOrigin(
      pane,
      details?.isMainFrame === true,
      details?.requestingUrl,
      details?.securityOrigin,
    );
    const outcome = permissions.resolve({
      electronName: permission,
      details,
      kind: "request",
      ...policyInputs(pane, origin),
    });
    if (outcome.verdict !== "ask") {
      // Including the allow, at debug volume: a permission path that answers
      // silently in every direction is one where "the config does not work" and
      // "Chromium refused for its own reasons" look identical from the page.
      if (outcome.verdict === "deny") {
        warnDenied(permission, `policy (${outcome.source}) for ${permissions.originKey(origin)}`);
      }
      return callback(outcome.verdict === "allow");
    }

    void askUser(window, pane.viewId, pane.entry, outcome.ids, origin, details)
      .catch((error) => {
        // Electron is holding a callback: a throw anywhere above must still become
        // an answer, or the page waits forever. This is how one undefined constant
        // hung every prompted permission.
        console.warn("[veld] permission prompt failed", error);
        return null;
      })
      .then((answer) => {
      if (answer === null) {
        // Nobody answered. Refuse this request — the page is waiting — but record
        // nothing: a remembered "deny" here is indistinguishable from one the user
        // chose, and it outranks the project config permanently.
        warnDenied(permission, "the prompt was dismissed without an answer");
        return callback(false);
      }
      // Remembered *before* answering: `setDisplayMediaRequestHandler` runs
      // straight after this for a `getDisplayMedia` call and re-resolves the
      // policy, and it must find the answer already there or it prompts again for
      // the same call.
      for (const id of outcome.ids) {
        recordAnswer(partitionFor(pane.entry.profile), origin, id, answer);
      }
      persistPermissions();
      pushPermissionState(window, pane.viewId, pane.entry);
      callback(answer === "allow");
    });
  });

  ses.setPermissionCheckHandler((wc, permission, requestingOrigin, details) => {
    const pane = paneOf(wc);
    if (!pane) return false;
    const origin = requestOrigin(
      pane,
      details?.isMainFrame === true,
      requestingOrigin,
      details?.requestingUrl,
      details?.securityOrigin,
    );
    const outcome = permissions.resolve({
      electronName: permission,
      details,
      kind: "check",
      ...policyInputs(pane, origin),
    });
    // "ask" is `false` here — this handler has no way to prompt. See the note at
    // the top of this section.
    return outcome.verdict === "allow";
  });

  // Without this handler Electron rejects `getDisplayMedia` outright, whatever
  // the permission says: there is no built-in picker for it to fall back on. The
  // grant is deliberately narrow — the requesting frame itself, which is exactly
  // what `preferCurrentTab` asks for, and what veld's own feedback overlay uses.
  // Nothing outside the pane is ever offered, so there is no picker to show.
  ses.setDisplayMediaRequestHandler((request, callback) => {
    const wc = request.frame ? allWebContents.fromFrame(request.frame) : null;
    const pane = paneOf(wc);
    if (!pane) {
      warnDenied("display-capture", "no pane owns the requesting frame");
      return callback({});
    }
    // The display-media request carries a frame rather than a flag, so the
    // main-frame test is the frame itself.
    const paneWc = pane.entry.view.webContents;
    const fromMainFrame = !paneWc.isDestroyed() && request.frame === paneWc.mainFrame;
    const origin = requestOrigin(pane, fromMainFrame, request.securityOrigin);
    const outcome = permissions.resolve({
      electronName: "display-capture",
      kind: "request",
      origin,
      ...policyInputs(pane, origin),
    });
    if (outcome.verdict !== "allow") {
      // The one place a denial is worth a line even when the policy meant it:
      // `veld feedback` screenshots run through here, and "the overlay says
      // denied" is otherwise a dead end.
      warnDenied("display-capture", `policy says ${outcome.verdict} for ${permissions.originKey(origin)}`);
      return callback({});
    }
    // **A gesture is required even when the answer is already yes.** A stored
    // Allow outranks the policy forever, so without this one click on one
    // screenshot became a standing, silent, script-callable capture of the pane
    // at that origin — a page could grab a frame on load. Browsers require
    // transient activation for `getDisplayMedia` for exactly this reason, and
    // Electron reports it on the request.
    if (!request.userGesture) {
      warnDenied("display-capture", "no user gesture — capture must follow a click or keypress");
      return callback({});
    }
    callback({
      video: request.frame,
      // Tab audio travels with the capture the way it does in a browser, and only
      // when the page asked for it.
      ...(request.audioRequested ? { audio: request.frame } : {}),
    });
  });
}

/** One line on stderr when a permission is refused for a structural reason. */
function warnDenied(permission, why) {
  console.warn(`[veld] permission "${permission}" denied: ${why}`);
}

function denyUnattributable(callback, permission, why) {
  warnDenied(permission, why);
  callback(false);
}

/** Tell a pane's chrome what its current site is allowed to do. */
function pushPermissionState(window, viewId, entry) {
  if (window.isDestroyed() || entry.view.webContents.isDestroyed()) return;
  const origin = permissions.parseOrigin(entry.view.webContents.getURL());
  const { rules, trustedOrigins } = policyFor(window.id);
  send(window, "veld:browser:permissions", {
    viewId,
    origin: permissions.originKey(origin),
    settings: origin
      ? permissions.siteSettings({
          origin,
          rules,
          trustedOrigins,
          stored: permissionStore[partitionFor(entry.profile)] ?? {},
        })
      : [],
  });
}

function disposeEntry(window, entry, viewId) {
  // A prompt this pane raised can no longer be answered — its chrome is going
  // away — and the page behind it is blocked on the callback.
  if (viewId !== undefined) abandonPrompts({ windowId: window.id, viewId });
  // A view disposed mid-gesture must not stay armed: nothing else clears this once the
  // entry is unreachable, and a forwarding view costs a cursor read and an IPC message
  // per mouse move.
  entry.dragging = false;
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
  // Before the views go: a prompt this window was showing can no longer be
  // answered, and the page behind it is blocked on the callback.
  abandonPrompts({ windowId: window.id });
  policyByWindow.delete(window.id);
  const entries = byWindow.get(window.id);
  if (!entries) return;
  for (const [viewId, entry] of entries) disposeEntry(window, entry, viewId);
  byWindow.delete(window.id);
}

/**
 * Register the IPC surface. Call once, before any window is created.
 *
 * `resolveWindow` maps an IPC event to the window that may own views — the
 * caller's own `BrowserWindow`. Handlers that cannot resolve one do nothing:
 * a message from a destroyed window is a race, not an error worth throwing at
 * a renderer that is already gone.
 *
 * `opts.permissionsFile` is where per-site permission answers live. Passed in
 * rather than derived here so this module stays testable and so the path is
 * decided in one place with the other `userData` files.
 */
function registerBrowserViewIpc(resolveWindow, opts = {}) {
  permissionsFile = opts.permissionsFile ?? null;
  permissionStore = loadPermissions();
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
    // The page's own theme surface, not white: this is what shows before the guest paints
    // and at the screen's rounded corners, and a white flash in a dark app is exactly
    // where an embedded view stops looking embedded. Falls back to white, which is what
    // every browser does with no better answer.
    view.setBackgroundColor(safeColor(args?.background) ?? "#ffffff");
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
      // Media overrides arrive with `create` for the same reason the emulation
      // does: a pane switching session recreates its view, and a dark-mode
      // override that came back a round trip late is visible as the page
      // painting light and then flipping.
      media: safeMedia(args?.media),
      zoom: safeZoom(args?.zoom) ?? 1,
      // The session's own UA, captured before anything overrides it — what
      // "no emulated device" has to restore.
      defaultUserAgent: view.webContents.getUserAgent(),
      scale: 1,
      radius: 0,
      // The /ide page's own zoom factor, which converts everything the renderer
      // measures in CSS pixels into the DIP this view lives in. Seeded from the
      // window rather than left at 1: a window restored on a remembered per-origin
      // zoom fires no event, so the first emulation would otherwise be applied at
      // the wrong factor and stay there until something moved.
      hostZoom: zoomFactor(window.webContents.getZoomFactor()),
      // The last box the renderer sent, in its own CSS pixels, so a zoom change can
      // re-place the view without waiting for the page to re-measure.
      cssRect: null,
      // What `enableDeviceEmulation` / `setBorderRadius` were last given, in DIP —
      // the applied values, which move with the zoom while the renderer's do not.
      metricsScale: null,
      appliedRadius: 0,
      touchActive: false,
      mediaActive: false,
      // Nothing has navigated yet, so the emulation calls are not safe to make —
      // rule 2. The first `did-navigate` flips this and applies them.
      frameReady: false,
      emulated: false,
      // Set only while the page is dragging this screen's edge; see the
      // `input-event` listener for what it turns on and why.
      dragging: false,
    };
    entries.set(viewId, entry);
    window.contentView.addChildView(view);
    attachListeners(window, viewId, entry);
    // The user agent goes on before the first load, so the page's first request
    // carries it rather than needing a reload to see it. The metrics wait for the
    // navigation `loadURL` is about to start.
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
    // Kept in the renderer's own CSS pixels as well as converted, because a later
    // zoom change has to re-convert the same box (see `syncWindowZoom`) and the
    // rounded DIP rect cannot be turned back into it.
    const css = {
      x: Number(r.x),
      y: Number(r.y),
      width: Number(r.width),
      height: Number(r.height),
    };
    if (!Object.values(css).every(Number.isFinite)) return;
    // Page zoom scales the renderer's CSS pixels and not a native view's bounds, so
    // without this conversion a zoomed /ide puts every native view over the wrong
    // region — the failure the renderer's geometry mirroring exists to prevent.
    const zoom = zoomFactor(event.sender.getZoomFactor());
    const rect = cssBoxToDip(css, zoom);
    // A zero or negative box is what a hidden or mid-layout pane reports; keep
    // the last good bounds and let visibility do the hiding, so returning to
    // the tab doesn't flash a 1px view.
    if (rect.width < 1 || rect.height < 1) return;
    const { entry } = found;
    entry.cssRect = css;
    entry.hostZoom = zoom;
    entry.view.setBounds(rect);
    // The screen's shape travels with its box, and through the same conversion:
    // `scaledRadius` gave it in CSS pixels, `setBorderRadius` wants the view's own.
    entry.radius = safeRadius(args?.radius);
    applyRadius(entry);
    // Re-emulate only when the applied factor actually moves: the renderer coalesces
    // a splitter drag to one push per frame, and `enableDeviceEmulation` relayouts
    // the guest page, so re-sending an unchanged scale would do that per frame.
    // Compared against what was *applied* rather than what was last received, so a
    // page zoom — which moves the applied factor while the renderer's number stands
    // still — is not mistaken for "nothing changed".
    //
    // The renderer's factor is recorded whether or not a device is set. Recording it
    // only while emulating meant a pane that spent a while at "Pane size" kept the
    // *previous* device's factor, and the next `emulate` applied metrics with it —
    // corrected only by luck, when the rect happened to change in the same breath.
    entry.scale = safeScale(args?.scale);
    if (entry.emulation && emulationScale(entry.scale, zoom) !== entry.metricsScale) {
      applyMetrics(entry);
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

  /**
   * Emulate `prefers-color-scheme` and friends for one pane.
   *
   * The same question a device width asks, put to a media feature — and the one
   * part of emulation Electron has no API for, so it goes over CDP like touch
   * does. No reload: unlike the user agent, a media feature is a live media query
   * and Chromium re-evaluates it, which is the whole reason this reads as
   * flipping the page's theme rather than reloading it into another one.
   */
  ipcMain.handle("veld:browser:media", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const { window, entry } = found;
    if (entry.view.webContents.isDestroyed()) return;
    entry.media = safeMedia(args?.media);
    // Through the same queue as touch, because they share one debugger session.
    void applyTouch(window, args.viewId, entry);
    pushState(window, args.viewId, entry);
  });

  /**
   * Start or stop forwarding this view's pointer events to the page.
   *
   * On only for the duration of a resize drag: it is one IPC message per mouse move
   * *while the cursor is over the pane's own page*, which is a cost worth paying to
   * keep a gesture alive and not worth paying at any other time.
   */
  ipcMain.handle("veld:browser:drag", (event, args) => {
    // Resolved from the *window*, with no view id involved. Requiring one meant the
    // disarm was dropped whenever the pane that started the gesture had gone — closed
    // mid-drag, or rebuilt by a session switch — and since arming is window-wide, every
    // view then kept forwarding a cursor read plus an IPC message per mouse move until
    // the page reloaded.
    const window = senderWindow(event);
    if (!window) return;
    const dragging = args?.dragging === true;
    // **Every view in the window, not just the dragged one.** A pointer released
    // over a *sibling* pane's view reaches neither this document's `pointerup` (that
    // view owns its own rect) nor the forwarded channel if only the dragged view is
    // forwarding — so the gesture could never end: `resizing` stayed true, which
    // disables geometry sync for that view for the life of the page, and the
    // renderer's window listeners kept resizing the guest to a button-less cursor.
    // Two browser panes side by side is the documented layout, so this is a
    // straight-line path, not a corner. The coordinates are window-relative and come
    // from the cursor, so an event forwarded by any view is equally usable.
    for (const entry of byWindow.get(window.id)?.values() ?? []) {
      entry.dragging = dragging;
    }
  });

  /**
   * Repaint every view in the window on the page's theme surface.
   *
   * Window-wide and view-less, like `drag`: a theme switch is one event for the whole
   * app, and addressing it per view would mean the renderer walking its own registry to
   * say the same thing sixteen times.
   */
  ipcMain.handle("veld:browser:background", (event, args) => {
    const window = senderWindow(event);
    if (!window) return;
    const color = safeColor(args?.background);
    if (!color) return;
    for (const entry of byWindow.get(window.id)?.values() ?? []) {
      if (!entry.view.webContents.isDestroyed()) entry.view.setBackgroundColor(color);
    }
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
   * An attached debugger is **left alone** here. Electron's docs say the inspector
   * takes the CDP session, and this handler used to detach first so the pane would
   * hear about it in the same round trip — but on Electron 43 the two coexist, so
   * that was throwing touch emulation away to avoid a conflict that does not
   * happen. If a version does take the session, the `detach` listener in
   * `attachListeners` reports it and `devtools-closed` retries.
   */
  ipcMain.handle("veld:browser:devtools", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const { window, entry } = found;
    const wc = entry.view.webContents;
    if (wc.isDestroyed()) return;
    const action = args?.action;
    const open = action === "open" || (action === "toggle" && !wc.isDevToolsOpened());
    if (open) wc.openDevTools({ mode: "detach", activate: true });
    else wc.closeDevTools();
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
   * Find-in-page, driven by the pane's own find bar.
   *
   * `action` is "start" (a fresh search — the page's own current text, not
   * whatever a previous query matched), "next"/"previous" (step to another match
   * of the same text) or "stop" (clear the highlights). This always calls
   * through to the real `WebContents` — there is no "frozen" mode for a pane's
   * page content, only a paint substitute the renderer shows while a view is
   * hidden, so a search issued against a suspended pane still finds the page's
   * actual, current text and the highlights are there the moment it is shown
   * again.
   */
  ipcMain.handle("veld:browser:find", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const wc = found.entry.view.webContents;
    if (wc.isDestroyed()) return;
    if (args?.action === "stop") {
      wc.stopFindInPage("clearSelection");
      return;
    }
    const text = typeof args?.text === "string" ? args.text : "";
    if (!text) {
      wc.stopFindInPage("clearSelection");
      return;
    }
    // One options object per branch rather than a bare call for "start": a
    // field added later (a match-case toggle, say) then has three explicit
    // places to land instead of one bare call that's easy to miss. An
    // unrecognized action is a safe no-op, matching `command`'s own
    // explicit-whitelist switch above rather than guessing at what "start" means.
    let options;
    switch (args?.action) {
      case "start":
        options = {};
        break;
      case "next":
        options = { forward: true, findNext: true };
        break;
      case "previous":
        options = { forward: false, findNext: true };
        break;
      default:
        return;
    }
    wc.findInPage(text, options);
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
    disposeEntry(found.window, found.entry, args.viewId);
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
   *
   * **Every window's panes, not the sender's.** A partition is process-wide —
   * `session.fromPartition` is not scoped to a window — so the clear already
   * reached them all; scoping only the *repair* to one window left the others
   * showing a signed-in page backed by a jar that no longer exists. That was
   * unreachable while the app had one window and became reachable the moment it
   * could have several.
   */
  ipcMain.handle("veld:browser:clear-session", async (event, args) => {
    if (!senderWindow(event)) return;
    const profile = typeof args?.profile === "string" ? args.profile : "";
    if (!isProfileName(profile)) throw new Error("invalid profile name");
    await session.fromPartition(partitionFor(profile)).clearStorageData();
    for (const entries of byWindow.values()) {
      for (const entry of entries.values()) {
        if (entry.profile !== profile) continue;
        if (!entry.view.webContents.isDestroyed()) entry.view.webContents.reload();
      }
    }
    // Permissions the user granted this session go with it. A grant that
    // survived "sign out of this session" would be the one piece of "this site
    // knows me" the clear silently missed.
    permissionStore = permissions.forgetPartition(permissionStore, partitionFor(profile));
    clearedPartitions.add(partitionFor(profile));
    // The whole partition is going, so its per-answer revocations are redundant —
    // and leaving them would keep re-deleting answers given after a later re-grant.
    for (const key of [...revoked]) {
      if (key.startsWith(`${partitionFor(profile)}\u0000`)) revoked.delete(key);
    }
    persistPermissions();
    for (const [windowId, entries] of byWindow) {
      const window = windowById(windowId);
      if (!window) continue;
      for (const [viewId, entry] of entries) {
        if (entry.profile === profile) pushPermissionState(window, viewId, entry);
      }
    }
  });

  // -- Permissions ----------------------------------------------------------

  /**
   * The policy inputs for this window: the selected worktree's `ide.permissions`,
   * and the origins veld itself serves for its run.
   *
   * Pushed by the renderer rather than fetched here, because the renderer is what
   * knows which worktree the window is showing and already holds `/api/repos`.
   * Everything in it is treated as untrusted: the rules are re-validated below,
   * and the trusted origins are normalised through the same parser the matcher
   * uses, so a malformed entry cannot widen anything.
   */
  ipcMain.handle("veld:browser:policy", (event, args) => {
    const window = senderWindow(event);
    if (!window) return;
    const rules = Array.isArray(args?.rules) ? args.rules.filter((rule) => isPermissionRule(rule, permissions.VELD_PERMISSIONS)) : [];
    // Filtered to this machine, not merely parsed. These origins get
    // `display-capture` with no prompt on the premise that they are "origins veld
    // itself serves" — but they are built from the project's own `url_template`,
    // and a non-`.localhost` template is only *warned* about by `veld start`,
    // never refused. Without this filter a repo could put a public origin into
    // the silent-capture set by editing one config line.
    const trustedOrigins = Array.isArray(args?.trustedOrigins)
      ? args.trustedOrigins
          .map((url) => permissions.parseOrigin(url))
          .filter((origin) => origin !== null && permissions.isLocalOrigin(origin))
          .map((origin) => permissions.originKey(origin))
      : [];
    policyByWindow.set(window.id, { rules, trustedOrigins });
    // The panel is showing verdicts computed from the previous policy.
    for (const [viewId, entry] of entriesFor(window.id)) {
      pushPermissionState(window, viewId, entry);
    }
  });

  /** A user's answer to a prompt this window raised. */
  ipcMain.handle("veld:browser:permission-reply", (event, args) => {
    const window = senderWindow(event);
    if (!window) return;
    const requestId = Number(args?.requestId);
    if (!Number.isInteger(requestId)) return;
    settlePrompt(window.id, requestId, args?.verdict === "allow" ? "allow" : "deny");
  });

  /**
   * The UI is dropping a prompt without an answer — a second request arriving
   * while one is already up, or the page navigating out from under it.
   *
   * Distinct from a reply, and that is the whole point: the renderer previously
   * had **no way** to release a prompt, so its only options were to answer for
   * the user (writing a permanent verdict they never chose) or to drop it on the
   * floor, leaving the page blocked on a callback that could never fire. Resolves
   * `null`, so the request is refused and nothing is remembered.
   */
  ipcMain.handle("veld:browser:permission-abandon", (event, args) => {
    const window = senderWindow(event);
    if (!window) return;
    const requestId = Number(args?.requestId);
    if (!Number.isInteger(requestId)) return;
    const pending = pendingPrompts.get(requestId);
    if (!pending || pending.windowId !== window.id) return;
    releasePrompt(requestId, pending);
    pending.resolve(null);
  });

  /**
   * Set (or clear) one permission from the per-site panel.
   *
   * `verdict: "default"` removes the user's answer and lets the project config or
   * veld's default answer again — which is what makes the panel's third state
   * meaningful rather than a disguised deny.
   */
  ipcMain.handle("veld:browser:set-permission", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    const origin = permissions.parseOrigin(args?.origin);
    if (!origin) return;
    const id = typeof args?.permission === "string" ? args.permission : "";
    const verdict = args?.verdict === "allow" || args?.verdict === "deny" ? args.verdict : "default";
    recordAnswer(partitionFor(found.entry.profile), origin, id, verdict);
    persistPermissions();
    pushPermissionState(found.window, args.viewId, found.entry);
  });

  /** The panel asking for the current site's state — a cold open needs it. */
  ipcMain.handle("veld:browser:permissions", (event, args) => {
    const found = lookup(event, args?.viewId);
    if (!found) return;
    pushPermissionState(found.window, args.viewId, found.entry);
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

module.exports = { registerBrowserViewIpc, disposeWindow, syncWindowZoom };
