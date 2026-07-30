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

import { type PaneEmulation, DEFAULT_ZOOM, clampZoom, fitScale } from "./devices";
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
  /**
   * How far a fitted emulated viewport ended up scaled, 1 when it fits or when
   * there is no emulation.
   *
   * Reported by the backend rather than computed here because under Electron it
   * is a function of the native view's box in device-independent pixels, which
   * only the main process knows (page zoom scales the renderer's CSS pixels and
   * not the view's bounds). The pane shows it — "1440 × 900 at 42%" — because a
   * scaled-down device is otherwise indistinguishable from a small one.
   */
  emulationScale: number;
  /**
   * Whether touch emulation is *actually* in force.
   *
   * Separate from the `touch` the pane asked for, because Chromium hands the
   * built-in DevTools an exclusive CDP session and touch needs that same session:
   * opening a pane's inspector suspends its touch emulation until it closes. The
   * pane says so rather than claiming a mode it does not have.
   */
  touchActive: boolean;
  /** Whether this pane's (detached) DevTools window is open. */
  devToolsOpen: boolean;
}

/** The bridge the desktop shell injects (desktop/src/preload.js). */
interface DesktopBrowserApi {
  create(
    viewId: string,
    options: {
      url?: string;
      profile: BrowserProfile;
      /** Sent with `create` rather than after it: a pane switching session
       *  recreates its view, and a device arriving a round trip late is visible
       *  as the page laying out at pane size and then jumping. */
      emulation: PaneEmulation | null;
      zoom: number;
    },
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
  emulate(viewId: string, emulation: PaneEmulation | null): Promise<void>;
  setZoom(viewId: string, zoom: number): Promise<void>;
  devTools(viewId: string, action: "toggle" | "open" | "close"): Promise<void>;
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
  /**
   * Whether the shell currently holds a view for this id.
   *
   * `create` can legitimately fail — the per-window cap is reachable, since a
   * layout is kept for every worktree visited and each one opens with a browser
   * pane. Without this flag a failed create left a renderer-side view whose every
   * later call was a silent no-op in the shell (`navigate` and `command` return
   * quietly on an unknown id), so the pane cleared its error on "Try again" and
   * then hung on the spinner with no way out but closing the tab.
   */
  shellHasView: boolean;
  /** Bumped per freeze request, so a capture that lands after the pane came
   *  back is discarded instead of freezing a live page. */
  freezeGeneration: number;
  /**
   * Last visibility pushed to the shell, so unchanged answers cost no IPC.
   *
   * Starts `true` to match the shell, which creates views visible. Be precise
   * about what that buys: the *load* starts against a visible view, but the very
   * next `applyVisibility` hides it again, because a first load is `covered()` —
   * the spinner is DOM, and a native view would paint over it. So this avoids
   * create-hidden-and-load-in-one-tick; it does not keep the view visible while
   * the page arrives, and it cannot, as long as the pane draws its own spinner.
   */
  visible: boolean;
  /**
   * The device this view emulates, and its page zoom.
   *
   * The layout is the record (`PaneTab.emulation` / `PaneTab.zoom`); these are the
   * live copies, kept because **the view can be recreated underneath them** — a
   * session switch destroys and rebuilds one, as does recovering from a refused
   * `create` — and both are per-`WebContents` state that has to be re-asserted
   * when that happens. Same shape as `url`, for the same reason.
   */
  emulation: PaneEmulation | null;
  zoom: number;
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

/**
 * Ask the shell for a view, recording whether it now has one.
 *
 * Retried from [`navigateBrowser`] and [`reloadBrowser`] rather than only on
 * creation, so a failure that has since cleared (a pane closed, freeing a slot
 * under the per-window cap) is recoverable from the pane's own "Try again".
 */
async function createShellView(v: View, url: string | undefined): Promise<boolean> {
  if (!desktop) return true;
  try {
    // A *resolved* create is not a successful one: the shell answers `null` when it
    // cannot resolve the sender to its window's main frame, and the bridge types
    // this `Promise<unknown>`, so nothing else would notice. Believing it would set
    // `shellHasView` for a view that does not exist and re-open the silent
    // spinner-forever hang this whole path exists to prevent.
    // The emulation and the zoom go in with the create, so a rebuilt view is
    // never briefly the wrong device: the shell applies both *before* the first
    // `loadURL`, which is also what keeps the emulated user agent on the page's
    // first request instead of costing a reload.
    if (
      !(await desktop.create(v.id, {
        url,
        profile: v.profile,
        emulation: v.emulation,
        zoom: v.zoom,
      }))
    ) {
      throw new Error("the desktop shell refused this pane");
    }
    v.shellHasView = true;
    // Forget what the *failed* attempt mirrored. Both caches were written while no
    // shell entry existed, so their sends were dropped: `v.rect` by a `syncBounds`
    // that went nowhere, `v.visible` by the `applyVisibility` in `mountBrowser`.
    // Leaving them meant the retried view received neither `setBounds` (the rect
    // compares equal, so `syncBounds` early-returns) nor `setVisible` — a view at
    // its default bounds, which is the blank pane this retry path exists to fix,
    // reintroduced one step later.
    v.rect = null;
    v.visible = true;
    applyVisibility(v);
    scheduleGeometrySync();
    return true;
  } catch (e: unknown) {
    v.shellHasView = false;
    patch(v, {
      loading: false,
      error: localError(e instanceof Error ? e.message : String(e)),
    });
    applyVisibility(v);
    return false;
  }
}

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
  return paneCovers(v.state);
}

/**
 * The same rule, as a function of state alone, so the pane that renders those
 * screens and the code that hides the view cannot drift apart.
 *
 * They were two expressions of one invariant with no shared code and no test —
 * and a divergence is invisible in the browser build (z-index works there),
 * surfacing only in Electron as a screen painted under a live page, or a pane that
 * stays blank. `fallbackUrl` is the tab's stored URL, which the pane knows on its
 * first render, before any view exists.
 */
export function paneCovers(state: BrowserState, fallbackUrl?: string): boolean {
  if (state.error) return true;
  if (!state.url && !fallbackUrl) return true;
  return !state.loaded && state.loading;
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
  // Swallowed deliberately: visibility is re-asserted by every later mount,
  // suspend and state event, so one lost call self-corrects — unlike a navigation.
  void desktop.setVisible(v.id, next).catch(() => {});
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
      emulationScale: 1,
      touchActive: false,
      devToolsOpen: false,
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

/** Everything a view needs when it is created — and, because a session switch
 *  recreates one, everything that has to be re-asserted when it is. */
export interface BrowserViewOptions {
  url?: string;
  profile: BrowserProfile;
  emulation?: PaneEmulation | null;
  zoom?: number;
}

function ensure(id: string, options: BrowserViewOptions): View {
  const { url, profile } = options;
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
      emulationScale: 1,
      touchActive: false,
      devToolsOpen: false,
    },
    listeners: new Set(),
    observer: null,
    mounted: false,
    rect: null,
    shellHasView: !desktop,
    freezeGeneration: 0,
    visible: true,
    emulation: options.emulation ?? null,
    zoom: clampZoom(options.zoom ?? DEFAULT_ZOOM),
  };
  views.set(id, v);
  const waiting = pending.get(id);
  if (waiting) {
    for (const fn of waiting) v.listeners.add(fn);
    pending.delete(id);
  }

  if (desktop) {
    void createShellView(v, url);
  } else {
    const frame = document.createElement("iframe");
    frame.className = "browser-frame";
    // No `sandbox`: the pane previews the user's *own* application, and a
    // sandbox without allow-same-origin gives it an opaque origin — no cookies,
    // no localStorage, no logged-in session, which is the whole point of the
    // pane. The frame is cross-origin to /ide either way, which is what keeps it
    // out of this document.
    // No `allow`: `clipboard-write` is `self` by default, and delegating it to
    // arbitrary previewed content lets one click overwrite the user's clipboard.
    // The Electron backend denies every permission, so granting one here would be
    // the browser build being *less* careful than the desktop one.
    frame.addEventListener("load", () => {
      // `reloadBrowser` bounces this frame through `about:blank`, which fires the
      // same event — treating that as the page having arrived cleared the loading
      // indicator one navigation early.
      if (frame.src === "about:blank") return;
      patch(v, { loading: false, loaded: true });
    });
    if (url) frame.src = url;
    container.appendChild(frame);
    v.iframe = frame;
    applyIframeEmulation(v);
  }
  return v;
}

/** Mount a view into `parent`, creating it on first call. */
export function mountBrowser(id: string, parent: HTMLElement, options: BrowserViewOptions): void {
  const v = ensure(id, options);
  if (v.container.parentElement !== parent) parent.appendChild(v.container);
  v.mounted = true;
  applyVisibility(v);
  // Observed under both backends: the native view mirrors the box, and a fitted
  // emulated viewport is scaled to it, so either way the answer changes with the
  // pane's size.
  if (!v.observer) {
    v.observer = new ResizeObserver(scheduleGeometrySync);
    v.observer.observe(v.container);
  }
  scheduleGeometrySync();
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
    void (async () => {
      // A view the shell never created (or has since destroyed) would swallow this
      // silently, so ask for one first and let the failure surface on the pane.
      if (!v.shellHasView && !(await createShellView(v, url))) return;
      await desktop.navigate(id, url).catch(reportFailure(v));
    })();
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
  // `loading` only if there is something to load. A blank pane has no navigation
  // to clear the flag, so an unconditional `true` left a live spinner and an
  // enabled Stop over the URL launcher — a state a freshly opened blank pane
  // never shows.
  patch(v, { loading: Boolean(v.state.url), error: null });
  applyVisibility(v);
  if (desktop) {
    void (async () => {
      if (!v.shellHasView) {
        // "Try again" after a failed create is the one place a pane can recover
        // from the shell refusing it, so it has to be a real retry.
        await createShellView(v, v.state.url || undefined);
        return;
      }
      await desktop.command(id, "reload").catch(reportFailure(v));
    })();
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
  // Reported on every pane using the slot: the menu item claims to sign them out,
  // so a refused or failed clear must not look like it worked.
  void desktop.clearSession(profile).catch((e: unknown) => {
    for (const v of views.values()) {
      if (v.profile === profile) reportFailure(v)(e);
    }
  });
}

/**
 * Point a pane at a device, or at nothing (`null` — the pane at pane size).
 *
 * The live copy is written **before** the call, and unconditionally: a view the
 * shell has not created yet (a refused `create` the pane can still retry from
 * "Try again") must come back as the device the pane is set to, not as the one it
 * was opened with. That is what makes this survive a session switch, which
 * destroys and rebuilds the view.
 */
export function setBrowserEmulation(id: string, emulation: PaneEmulation | null): void {
  const v = views.get(id);
  if (!v) return;
  v.emulation = emulation;
  if (!desktop) {
    applyIframeEmulation(v);
    return;
  }
  // Nothing to talk to yet; `createShellView` sends it with the create.
  if (!v.shellHasView) return;
  // Surfaced rather than swallowed: the pane's chrome has already changed to say
  // it is emulating a phone, so a refused call has to be visible or the chrome is
  // lying about what the page is being shown as.
  void desktop.emulate(id, emulation).catch(reportFailure(v));
}

/** Page zoom for one pane. Stored live for the same recreation reason as the
 *  emulation; under the iframe backend there is no zoom to set (see
 *  [`applyIframeEmulation`]). */
export function setBrowserZoom(id: string, zoom: number): void {
  const v = views.get(id);
  if (!v) return;
  v.zoom = clampZoom(zoom);
  if (!desktop || !v.shellHasView) return;
  void desktop.setZoom(id, v.zoom).catch(reportFailure(v));
}

/** Open, close or toggle this pane's DevTools — always detached; see the shell's
 *  handler for why a docked inspector cannot work here. */
export function browserDevTools(id: string, action: "toggle" | "open" | "close"): void {
  const v = views.get(id);
  if (!v || !desktop || !v.shellHasView) return;
  void desktop.devTools(id, action).catch(reportFailure(v));
}

export function focusBrowser(id: string): void {
  const v = views.get(id);
  if (!v) return;
  if (desktop) void desktop.command(id, "focus").catch(() => {});
  else v.iframe?.focus();
}

export function disposeBrowser(id: string): void {
  const v = views.get(id);
  if (!v) return;
  v.observer?.disconnect();
  v.container.remove();
  v.listeners.clear();
  views.delete(id);
  v.shellHasView = false;
  if (desktop) void desktop.destroy(id).catch(() => {});
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
// Geometry
// ---------------------------------------------------------------------------

/**
 * Bring every mounted view's geometry back in line with its container, next frame.
 *
 * Two backends, one trigger, because both answers are a function of the pane's
 * box: Electron mirrors it onto the native view, and the iframe backend scales a
 * fitted emulated viewport to it.
 *
 * Coalesced to one frame for all views: a splitter drag fires `ResizeObserver`
 * per pointer move on both panes, and under Electron each push is an IPC round
 * trip. Deferring also gets the *settled* box — the one read mid-layout is
 * frequently zero.
 */
let framePending = false;
function scheduleGeometrySync(): void {
  if (framePending) return;
  framePending = true;
  requestAnimationFrame(() => {
    framePending = false;
    for (const view of views.values()) {
      if (desktop) syncBounds(view);
      else applyIframeEmulation(view);
    }
  });
}

/**
 * Size the frame to the emulated viewport and scale it to fit the pane.
 *
 * The honest half of emulation in a plain browser: an iframe's *own* width is a
 * real viewport, so a page laid out in a 390px frame sees 390px in its media
 * queries — the layout case, which is most of why anyone reaches for this. What a
 * frame has no API for is the rest: no user agent, no touch events, no device
 * pixel ratio, and no page zoom. The pane states that gap rather than implying
 * parity (`BrowserPane`'s device menu), and `browserBackend` is what it keys off.
 *
 * `transform` rather than `zoom`: scaling the *rendered result* is the point —
 * the frame keeps its 390 CSS pixels and merely takes up fewer of the pane's.
 */
function applyIframeEmulation(v: View): void {
  const frame = v.iframe;
  if (!frame) return;
  const e = v.emulation;
  if (!e) {
    // Back to the stylesheet's own 100%/100%, rather than to a computed size:
    // the pane can be resized while no emulation is set, and a leftover pixel
    // width would not follow it.
    frame.style.removeProperty("width");
    frame.style.removeProperty("height");
    frame.style.removeProperty("transform");
    frame.style.removeProperty("transform-origin");
    delete frame.dataset.emulated;
    patch(v, { emulationScale: 1 });
    return;
  }
  const box = v.container.getBoundingClientRect();
  const scale = fitScale(e, box);
  // The attribute is what releases the stylesheet's `inset: 0`, which would
  // otherwise over-constrain the box and win against the width below.
  frame.dataset.emulated = "true";
  frame.style.width = `${e.width}px`;
  frame.style.height = `${e.height}px`;
  frame.style.transform = `scale(${scale})`;
  frame.style.transformOrigin = "top left";
  patch(v, { emulationScale: scale });
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
  // Forget the cache if the send failed, or the poll below can never re-try it:
  // the rect is recorded *before* the send, and `syncBounds` early-returns while
  // the cached value matches — so a pane sitting still would keep a view at stale
  // bounds forever. (`applyVisibility` writes before sending too, but its value
  // flips on every suspend and resume, so it self-corrects.)
  void desktop.setBounds(v.id, rect).catch(() => {
    v.rect = null;
  });
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
    // What the pane cannot know for itself: how far a fitted viewport is scaled,
    // whether touch survived (DevTools takes its CDP session), and whether the
    // inspector is open. Each is read only when present, so an event about
    // something else cannot reset it.
    if (typeof payload.emulationScale === "number" && Number.isFinite(payload.emulationScale)) {
      next.emulationScale = payload.emulationScale;
    }
    if (typeof payload.touchActive === "boolean") next.touchActive = payload.touchActive;
    if (typeof payload.devToolsOpen === "boolean") next.devToolsOpen = payload.devToolsOpen;
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
