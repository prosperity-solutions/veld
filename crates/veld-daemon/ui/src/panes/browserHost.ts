/**
 * Live browser panes, owned outside React — the same discipline as
 * `terminalHost.ts` and for a weaker but real version of the same reason: a
 * remount would reload the page, losing scroll position, form state and
 * whatever the dev server had hot-reloaded into it. React unmounts on every tab
 * and worktree switch, so the pane's element lives in a module-level registry
 * keyed by tab id and the component only *reparents* it.
 *
 * ## Two backends
 *
 * **Electron** (`window.veldDesktop.browser`): a real `WebContentsView` in the
 * main process. Isolated cookie jars, working back/forward, page title, and it
 * renders pages that refuse to be framed.
 *
 * **A plain browser**: an `<iframe>`. Kept because "the web UI must stay fully
 * usable without Electron" is an invariant (desktop/ARCHITECTURE.md), and
 * honest about what it cannot do — a cross-origin frame exposes no URL, no
 * title and no history, and a page sending `X-Frame-Options: DENY` renders as a
 * blank pane with nothing observable to report. So the iframe backend reports
 * the URL *it was told to load*, disables back/forward, and the pane offers
 * "open in system browser" beside it.
 *
 * The backend is decided once, at module load: `veldDesktop.browser` either
 * exists or it doesn't, and a mid-session change is not a thing.
 */

import { type BrowserProfile, normalizeBrowserUrl } from "./model";

export type BrowserBackend = "electron" | "iframe";

/**
 * Why a load failed, structured rather than prose.
 *
 * The pane picks its icon and wording from `code` — "nothing is listening here"
 * and "that hostname does not resolve" are different problems with different
 * fixes, and formatting them into one sentence in the shell would force the
 * renderer to parse prose to tell them apart.
 */
export interface BrowserError {
  kind: "load" | "cert" | "crash";
  /** Chromium net error, when there is one (negative). */
  code: number | null;
  text: string;
  url: string;
}

export interface BrowserState {
  /** What the pane is showing. Under the iframe backend this is what we asked
   *  for, which is not necessarily where the frame ended up. */
  url: string;
  /** The page's own title, when the backend can see one. */
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  /** Why the pane is blank, when we know. */
  error: BrowserError | null;
  profile: BrowserProfile;
  /** Whether a page has ever committed in this view. Distinguishes "opening" —
   *  where a spinner is the honest thing to show — from "reloading", where the
   *  page underneath is still worth looking at. */
  loaded: boolean;
}

/** The bridge the desktop shell injects (desktop/src/preload.js). */
interface DesktopBrowserApi {
  create(
    viewId: string,
    options: { url?: string; profile: BrowserProfile },
  ): Promise<unknown>;
  setBounds(
    viewId: string,
    rect: { x: number; y: number; width: number; height: number },
  ): Promise<void>;
  setVisible(viewId: string, visible: boolean): Promise<void>;
  navigate(viewId: string, url: string): Promise<void>;
  command(
    viewId: string,
    command: "back" | "forward" | "reload" | "stop" | "focus",
  ): Promise<void>;
  reset(): Promise<void>;
  destroy(viewId: string): Promise<void>;
  clearSession(profile: BrowserProfile): Promise<void>;
  capture(viewId: string): Promise<string | null>;
  onState(fn: (payload: Record<string, unknown>) => void): () => void;
  onOpenRequest(
    fn: (payload: { viewId: string; url: string; profile: BrowserProfile }) => void,
  ): () => void;
  onAccelerator(fn: (payload: { accelerator: string }) => void): () => void;
}

declare global {
  interface Window {
    veldDesktop?: { shell?: string; version?: string; browser?: DesktopBrowserApi };
  }
}

// `typeof window` guarded so this module can be imported by the unit tests,
// which run without a DOM (`environment: "node"`).
const desktop: DesktopBrowserApi | null =
  typeof window === "undefined" ? null : (window.veldDesktop?.browser ?? null);

export const browserBackend: BrowserBackend = desktop ? "electron" : "iframe";

/**
 * Drop views a previous page left attached to this window, before creating any.
 *
 * At module load, so it is queued ahead of every `create` this document makes.
 * The shell used to do this off its own navigation event, which is a race against
 * the renderer — and losing it destroyed the view the new page had *just* asked
 * for, leaving the first browser pane after a reload permanently blank.
 */
if (desktop) void desktop.reset().catch(() => {});

interface View {
  id: string;
  profile: BrowserProfile;
  /**
   * The element the pane reparents.
   *
   * Under the iframe backend it *contains* the live frame. Under Electron it is
   * empty and exists only to be measured — the native view is positioned to
   * match its box, which is also why it must be a real laid-out element rather
   * than the React slot (that one is recreated on every remount).
   */
  container: HTMLDivElement;
  iframe: HTMLIFrameElement | null;
  state: BrowserState;
  listeners: Set<() => void>;
  observer: ResizeObserver | null;
  /** Whether a pane currently has this view on screen. */
  mounted: boolean;
  /** Last rect pushed to the shell, so the poll below only sends changes. */
  rect: { x: number; y: number; width: number; height: number } | null;
  /** Bumped per freeze request, so a capture that lands after the pane came
   *  back is discarded instead of freezing a live page. */
  freezeGeneration: number;
  /** Last visibility pushed to the shell, so unchanged answers cost no IPC.
   *  Starts `true` because the shell creates views visible — it must not be
   *  throttled while loading its first page (see `create` in browserViews.js). */
  visible: boolean;
}

const views = new Map<string, View>();

/**
 * Listeners for views that don't exist yet — same contract as
 * `subscribeTerminal`: a caller working from the layout rather than from a
 * mounted pane must not silently receive nothing.
 */
const pending = new Map<string, Set<() => void>>();

/**
 * Nesting depth of "a DOM overlay is open, hide the native views".
 *
 * A `WebContentsView` is a native sibling of the page, so it paints *over*
 * every menu, dialog and dropdown regardless of z-index. There is no CSS
 * answer; the view has to be hidden while an overlay that would land on it is
 * open (see `overlayGuard.ts`). A counter rather than a boolean because
 * overlays nest — a Select inside a Modal — and the inner one closing must not
 * bring the views back over the outer one.
 */
let suspendDepth = 0;

/** A failure raised on this side of the bridge, in the same shape as one the
 *  shell reports, so the pane has exactly one error path to render. */
function localError(text: string): BrowserError {
  return { kind: "load", code: null, text, url: "" };
}

/**
 * Surface a rejected bridge call on the pane instead of dropping it.
 *
 * Every command is issued after the pane has already been patched optimistically
 * (`loading: true, error: null`), so swallowing the rejection leaves a spinner
 * that never resolves and hides the error it replaced.
 */
function reportFailure(v: View): (e: unknown) => void {
  return (e: unknown) => {
    patch(v, {
      loading: false,
      error: localError(e instanceof Error ? e.message : String(e)),
    });
    applyVisibility(v);
  };
}

function notify(v: View): void {
  for (const fn of v.listeners) fn();
}

function patch(v: View, next: Partial<BrowserState>): void {
  const merged = { ...v.state, ...next };
  const changed = (Object.keys(next) as Array<keyof BrowserState>).some(
    (k) => v.state[k] !== merged[k],
  );
  if (!changed) return;
  v.state = merged;
  notify(v);
}

/**
 * Whether the pane is showing a screen of its own instead of the page.
 *
 * The native view has to be *hidden* for any of those to be visible at all —
 * it paints over DOM — so "the pane has something to say" and "the view is on
 * screen" are the same decision. Three cases: no page asked for yet (the
 * chooser), a failed load (the error screen), and the very first load (the
 * spinner; a *re*-load leaves the old page up, which is more useful than a
 * spinner over nothing).
 */
function covered(v: View): boolean {
  if (v.state.error) return true;
  if (!v.state.url) return true;
  return !v.state.loaded && v.state.loading;
}

/** Whether a mounted view should currently be on screen. */
function shouldShow(v: View): boolean {
  return v.mounted && suspendDepth === 0 && !covered(v);
}

/**
 * Push the view's visibility, if it has changed.
 *
 * Called from every path that can affect the answer — mount, unmount, suspend,
 * resume, and each state event from the shell — so without the comparison a page
 * that reports progress produces a `setVisible` IPC round trip per event, all
 * saying what the shell already knows.
 */
function applyVisibility(v: View): void {
  if (!desktop) return;
  const next = shouldShow(v);
  if (v.visible === next) return;
  v.visible = next;
  void desktop.setVisible(v.id, next);
}

/**
 * How long to wait for a still before hiding a view anyway.
 *
 * The still is painted *before* the view is hidden, not after: hiding first left
 * a visible flash of empty pane while the capture crossed IPC, which is the
 * flicker this whole mechanism exists to avoid. The cost is that the overlay
 * spends a frame or two behind a view that is still up, which nobody notices.
 * The cap is there so a capture that never answers cannot leave the overlay
 * permanently underneath.
 */
const FREEZE_TIMEOUT_MS = 150;

/**
 * Hide every native view while a DOM overlay is open, and bring them back when
 * the last one closes. No-op under the iframe backend, where z-index works.
 *
 * Each visible view is **captured first**: a pane that went blank every time a
 * menu opened read as broken, so the still is painted in the view's place and the
 * pane freezes instead.
 */
export function pushBrowserSuspend(): void {
  suspendDepth += 1;
  if (suspendDepth !== 1) return;
  for (const v of views.values()) {
    if (desktop && v.mounted && !covered(v)) void freezeThenHide(v);
    else applyVisibility(v);
  }
}

export function popBrowserSuspend(): void {
  if (suspendDepth === 0) return;
  suspendDepth -= 1;
  if (suspendDepth !== 0) return;
  for (const v of views.values()) {
    applyVisibility(v);
    thaw(v);
  }
}

/**
 * Paint a still of the view, then hide it.
 *
 * The still is set **straight on the container element and decoded first**, not
 * put into React state. Two reasons, both of which were visible as flicker: a
 * state change costs a render before anything can paint, and an `<img>` with a
 * data URL decodes *asynchronously* — so the view was already hidden a frame or
 * two before its replacement appeared. `decode()` moves that cost in front of the
 * hide, where nobody is looking at it.
 *
 * Bounded by [`FREEZE_TIMEOUT_MS`]: a capture that is slow or refused must not
 * hold the view up in front of the overlay, so the view is hidden either way and
 * a missing still degrades to the blank pane this replaced.
 */
async function freezeThenHide(v: View): Promise<void> {
  if (!desktop) return;
  const generation = ++v.freezeGeneration;
  const deadline = new Promise<null>((resolve) => {
    window.setTimeout(() => resolve(null), FREEZE_TIMEOUT_MS);
  });
  const image = await Promise.race([desktop.capture(v.id).catch(() => null), deadline]);
  // The pane came back, or was superseded, while the capture was in flight:
  // painting a still over a live view would freeze a page that is running.
  if (v.freezeGeneration !== generation) return;
  if (image && suspendDepth > 0) {
    await Promise.race([decoded(image), deadline]);
    if (v.freezeGeneration !== generation || suspendDepth === 0) return;
    v.container.style.backgroundImage = `url("${image}")`;
  }
  applyVisibility(v);
}

/** Resolve once the browser has the pixels, so painting it costs no frame. */
async function decoded(dataUrl: string): Promise<null> {
  try {
    const img = new Image();
    img.src = dataUrl;
    await img.decode();
  } catch {
    // Unsupported or a malformed capture: fall through and let it decode on
    // paint, which is the behaviour this optimises rather than requires.
  }
  return null;
}

/**
 * Drop the still, once the view is definitely back in front of it.
 *
 * Not immediately on resume: making a view visible is another IPC round trip, so
 * clearing in the same tick re-opens the gap from the other side. It sits behind
 * the view meanwhile, where it cannot be seen, and every screen that could expose
 * it (`covered`) is opaque.
 */
function thaw(v: View): void {
  if (!v.container.style.backgroundImage) return;
  const generation = v.freezeGeneration;
  window.setTimeout(() => {
    if (v.freezeGeneration !== generation || suspendDepth > 0) return;
    v.container.style.backgroundImage = "";
  }, 250);
}

export function browserStatus(id: string): BrowserState {
  return (
    views.get(id)?.state ?? {
      url: "",
      title: "",
      loading: false,
      canGoBack: false,
      canGoForward: false,
      error: null,
      profile: "default",
      loaded: false,
    }
  );
}

export function subscribeBrowser(id: string, fn: () => void): () => void {
  const v = views.get(id);
  if (v) {
    v.listeners.add(fn);
    return () => v.listeners.delete(fn);
  }
  const waiting = pending.get(id) ?? new Set<() => void>();
  waiting.add(fn);
  pending.set(id, waiting);
  return () => {
    waiting.delete(fn);
    if (waiting.size === 0) pending.delete(id);
    views.get(id)?.listeners.delete(fn);
  };
}

function ensure(id: string, url: string | undefined, profile: BrowserProfile): View {
  const existing = views.get(id);
  if (existing) {
    // A profile is a cookie jar, and a view is bound to one for life. Switching
    // therefore means a new view — done here rather than at the call site so
    // there is no path that changes the tab's profile and leaves the pane
    // running in the old jar while the menu says otherwise.
    if (existing.profile === profile) return existing;
    disposeBrowser(id);
  }

  const container = document.createElement("div");
  container.className = "browser-host";

  const v: View = {
    id,
    profile,
    container,
    iframe: null,
    state: {
      url: url ?? "",
      title: "",
      loading: Boolean(url),
      canGoBack: false,
      canGoForward: false,
      error: null,
      profile,
      loaded: false,
    },
    listeners: new Set(),
    observer: null,
    mounted: false,
    rect: null,
    freezeGeneration: 0,
    visible: true,
  };
  views.set(id, v);
  const waiting = pending.get(id);
  if (waiting) {
    for (const fn of waiting) v.listeners.add(fn);
    pending.delete(id);
  }

  if (desktop) {
    void desktop.create(id, { url, profile }).catch((e: unknown) => {
      patch(v, {
        loading: false,
        error: localError(e instanceof Error ? e.message : String(e)),
      });
    });
  } else {
    const frame = document.createElement("iframe");
    frame.className = "browser-frame";
    // No `sandbox`: the pane previews the user's *own* application, and a
    // sandbox without allow-same-origin gives it an opaque origin — no cookies,
    // no localStorage, no logged-in session, which is the whole point of the
    // pane. The frame is cross-origin to /ide either way, which is what keeps it
    // out of this document.
    frame.setAttribute("allow", "clipboard-write");
    frame.addEventListener("load", () => patch(v, { loading: false, loaded: true }));
    if (url) frame.src = url;
    container.appendChild(frame);
    v.iframe = frame;
  }
  return v;
}

/** Mount a view into `parent`, creating it on first call. */
export function mountBrowser(
  id: string,
  parent: HTMLElement,
  options: { url?: string; profile: BrowserProfile },
): void {
  const v = ensure(id, options.url, options.profile);
  if (v.container.parentElement !== parent) parent.appendChild(v.container);
  v.mounted = true;
  applyVisibility(v);
  if (!v.observer && desktop) {
    v.observer = new ResizeObserver(scheduleBoundsSync);
    v.observer.observe(v.container);
  }
  scheduleBoundsSync();
}

/** Detach the element without destroying the view. */
export function unmountBrowser(id: string): void {
  const v = views.get(id);
  if (!v) return;
  v.mounted = false;
  applyVisibility(v);
  v.container.remove();
}

export function navigateBrowser(id: string, raw: string): string | null {
  const v = views.get(id);
  if (!v) return null;
  const url = normalizeBrowserUrl(raw);
  if (!url) {
    patch(v, { error: localError(`Not an http(s) address: ${raw.trim()}`) });
    return null;
  }
  patch(v, { url, loading: true, error: null });
  applyVisibility(v);
  if (desktop) {
    void desktop.navigate(id, url).catch((e: unknown) => {
      patch(v, {
        loading: false,
        error: localError(e instanceof Error ? e.message : String(e)),
      });
    });
  } else if (v.iframe) {
    v.iframe.src = url;
  }
  return url;
}

export function browserCommand(id: string, command: "back" | "forward" | "stop"): void {
  const v = views.get(id);
  if (!v) return;
  if (desktop) {
    void desktop.command(id, command).catch(reportFailure(v));
  }
  // The iframe backend has no history to walk and nothing to cancel: the
  // buttons are disabled, and this is only reachable via the palette.
}

export function reloadBrowser(id: string): void {
  const v = views.get(id);
  if (!v) return;
  patch(v, { loading: true, error: null });
  applyVisibility(v);
  if (desktop) {
    void desktop.command(id, "reload").catch(reportFailure(v));
    return;
  }
  const frame = v.iframe;
  const target = v.state.url;
  if (!frame || !target) return;
  // Re-assigning the same `src` is not reliably a reload, and
  // `contentWindow.location.reload()` is blocked cross-origin. Bouncing through
  // `about:blank` is: it is a real navigation either way.
  frame.src = "about:blank";
  window.setTimeout(() => {
    frame.src = target;
  }, 0);
}

/**
 * Clear one session slot's cookies and storage, then reload any pane using it.
 *
 * Addressed by slot rather than by pane, so a session with no pane open can still
 * be emptied — that is what "remove this session" means when the slots
 * themselves are fixed. Electron only: an iframe's cookie jar is the browser's
 * own, and clearing it is not ours to do.
 */
export function clearBrowserSession(profile: BrowserProfile): void {
  if (!desktop) return;
  void desktop.clearSession(profile);
}

export function focusBrowser(id: string): void {
  const v = views.get(id);
  if (!v) return;
  if (desktop) void desktop.command(id, "focus");
  else v.iframe?.focus();
}

export function disposeBrowser(id: string): void {
  const v = views.get(id);
  if (!v) return;
  v.observer?.disconnect();
  v.container.remove();
  v.listeners.clear();
  views.delete(id);
  if (desktop) void desktop.destroy(id);
}

/** Dispose every view not in `keep` — the layouts are the record of which
 *  should exist, exactly as for terminals. */
export function pruneBrowsers(keep: Iterable<string>): void {
  const live = new Set(keep);
  for (const id of [...views.keys()]) {
    if (!live.has(id)) disposeBrowser(id);
  }
}

// ---------------------------------------------------------------------------
// Geometry (Electron only)
// ---------------------------------------------------------------------------

/**
 * Mirror every mounted container's box onto its native view, next frame.
 *
 * Coalesced to one frame for all views: a splitter drag fires `ResizeObserver`
 * per pointer move on both panes, and each push is an IPC round trip. Deferring
 * also gets the *settled* box — the one read mid-layout is frequently zero.
 */
let framePending = false;
function scheduleBoundsSync(): void {
  if (!desktop || framePending) return;
  framePending = true;
  requestAnimationFrame(() => {
    framePending = false;
    for (const view of views.values()) syncBounds(view);
  });
}

function syncBounds(v: View): void {
  if (!desktop || !v.mounted || !v.container.isConnected) return;
  const box = v.container.getBoundingClientRect();
  const rect = {
    x: Math.round(box.left),
    y: Math.round(box.top),
    width: Math.round(box.width),
    height: Math.round(box.height),
  };
  if (rect.width < 1 || rect.height < 1) return;
  const last = v.rect;
  if (last && last.x === rect.x && last.y === rect.y && last.width === rect.width && last.height === rect.height) {
    return;
  }
  v.rect = rect;
  void desktop.setBounds(v.id, rect);
}

/**
 * Catch geometry changes a `ResizeObserver` cannot see.
 *
 * A pane can *move* without changing size — a banner appears above the dock, the
 * rail animates, a CSS transition settles a frame after the observer fired. A
 * native view left behind is not a subtle glitch: it covers the wrong part of
 * the window. So the settled box is re-read on a slow tick, and IPC only
 * happens when it actually differs (`syncBounds`), which makes the idle cost one
 * `getBoundingClientRect` per visible pane per interval.
 */
if (desktop) {
  window.setInterval(() => {
    for (const v of views.values()) syncBounds(v);
  }, 400);
  window.addEventListener("resize", () => {
    for (const v of views.values()) syncBounds(v);
  });
}

// ---------------------------------------------------------------------------
// Shell → page events
// ---------------------------------------------------------------------------

if (desktop) {
  desktop.onState((payload) => {
    const id = typeof payload.viewId === "string" ? payload.viewId : "";
    const v = views.get(id);
    if (!v) return;
    const next: Partial<BrowserState> = {};
    if (typeof payload.url === "string" && payload.url !== "" && payload.url !== "about:blank") {
      next.url = payload.url;
    }
    if (typeof payload.title === "string") next.title = payload.title;
    if (typeof payload.loading === "boolean") next.loading = payload.loading;
    if (typeof payload.canGoBack === "boolean") next.canGoBack = payload.canGoBack;
    if (typeof payload.canGoForward === "boolean") next.canGoForward = payload.canGoForward;
    // `error` is tri-state on the wire: an object sets it, `null` clears it, and
    // absent means "this event says nothing about it" — a title change must not
    // wipe the reason the pane is blank.
    if (payload.error && typeof payload.error === "object") {
      next.error = payload.error as BrowserError;
    } else if (payload.error === null) {
      next.error = null;
    }
    // A committed page is what makes a reload keep showing the old one rather
    // than a spinner over nothing.
    if (next.url) next.loaded = true;
    patch(v, next);
    // Visibility follows the state: an error or a first load means the pane has
    // its own screen to show, and the view has to be out of the way for it.
    applyVisibility(v);
  });
}

/**
 * `target=_blank` inside a pane, and the accelerators a focused native view
 * would otherwise swallow. Both are subscribed by the app, which is the only
 * thing that can act on them (it owns the layout).
 */
export function onBrowserOpenRequest(
  fn: (req: { viewId: string; url: string; profile: BrowserProfile }) => void,
): () => void {
  return desktop ? desktop.onOpenRequest(fn) : () => {};
}

export function onBrowserAccelerator(fn: (accelerator: string) => void): () => void {
  return desktop ? desktop.onAccelerator((p) => fn(p.accelerator)) : () => {};
}
