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

import {
  type DeviceLayout,
  type PaneEmulation,
  type PaneMedia,
  DEFAULT_ZOOM,
  clampZoom,
  deviceLayout,
  scaledRadius,
} from "./devices";
import { type BrowserProfile, isVeldOwnUi, normalizeBrowserUrl } from "./model";
import type { PermissionId, PermissionRule } from "../api";

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
  /**
   * The URL of Veld's own UI that this pane refused to open, or `null`.
   *
   * Not an `error`: nothing failed, and the pane offers a way through (see
   * `isVeldOwnUi`). It is its own state because the screen it shows is its own —
   * with a different set of actions from "try again".
   */
  nested: string | null;
  profile: BrowserProfile;
  /** Whether a page has ever committed in this view. Distinguishes "opening" —
   *  where a spinner is the honest thing to show — from "reloading", where the
   *  page underneath is still worth looking at. */
  loaded: boolean;
  /**
   * How far a fitted emulated viewport ended up scaled, 1 when it fits or when
   * there is no emulation.
   *
   * Computed here, by `deviceLayout`, and pushed to the shell with the bounds —
   * the box and the factor are one calculation, and having the shell derive the
   * factor from the box it was given was two owners of one number. The pane shows
   * it ("1440 × 900 · 42%") because a scaled-down device is otherwise
   * indistinguishable from a small one.
   */
  emulationScale: number;
  /**
   * The emulated screen's box inside the pane's content area, in CSS pixels; the
   * full content area when no device is set.
   *
   * Published because the *resize handles* are React's — they have to sit exactly
   * on the screen's edges, and only `syncGeometry` knows where those are. Kept as
   * four numbers rather than a rect object so `patch`'s `!==` comparison still
   * works; a new object every frame would re-render every pane on every tick.
   */
  deviceX: number;
  deviceY: number;
  deviceWidth: number;
  deviceHeight: number;
  /** Whether a resize drag is in progress, which hides the native view so the
   *  renderer receives the pointer at all. */
  resizing: boolean;
  /**
   * Whether touch emulation is *actually* in force.
   *
   * Separate from the `touch` the pane asked for, because touch needs Chromium's
   * debugger session and something else can hold it. The pane says so rather than
   * claiming a mode it does not have.
   */
  touchActive: boolean;
  /** Whether the emulated media features are *actually* in force — same caveat as
   *  `touchActive`, and the same reason: they share one debugger session. */
  mediaActive: boolean;
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
      media: PaneMedia | null;
      zoom: number;
      /** The theme's content surface, so a view does not flash white in a dark app
       *  before the guest paints. */
      background: string;
    },
  ): Promise<unknown>;
  setBounds(
    viewId: string,
    rect: { x: number; y: number; width: number; height: number },
    /** The factor the emulated viewport is rendered at inside `rect`, and the
     *  screen's corner radius at that scale. Sent with the box because all three
     *  are one calculation and one moment: see `deviceLayout`. */
    scale: number,
    radius: number,
  ): Promise<void>;
  setVisible(viewId: string, visible: boolean): Promise<void>;
  navigate(viewId: string, url: string): Promise<void>;
  command(
    viewId: string,
    command: "back" | "forward" | "reload" | "stop" | "focus",
  ): Promise<void>;
  emulate(viewId: string, emulation: PaneEmulation | null): Promise<void>;
  /** Optional here, and in every permission method below: these arrived after
   *  the bridge did, and an older shell will not have them. See the note above
   *  the permission helpers. */
  setMedia?(viewId: string, media: PaneMedia | null): Promise<void>;
  setZoom(viewId: string, zoom: number): Promise<void>;
  devTools(viewId: string, action: "toggle" | "open" | "close"): Promise<void>;
  /** Window-wide: the shell resolves the window from the sender, so a disarm lands
   *  even when the view that started the gesture is already gone. */
  drag(dragging: boolean): Promise<void>;
  /** Window-wide: the surface a view shows before its guest paints. */
  setBackground(background: string): Promise<void>;
  reset(): Promise<void>;
  destroy(viewId: string): Promise<void>;
  clearSession(profile: BrowserProfile): Promise<void>;
  capture(viewId: string): Promise<string | null>;
  onState(fn: (payload: Record<string, unknown>) => void): () => void;
  onOpenRequest(
    fn: (payload: { viewId: string; url: string; profile: BrowserProfile }) => void,
  ): () => void;
  onAccelerator(fn: (payload: { accelerator: string }) => void): () => void;
  onPointer(
    fn: (payload: { viewId: string; type: string; x: number; y: number }) => void,
  ): () => void;
  /** The permission policy for every pane in this window: the selected worktree's
   *  `ide.permissions`, and the origins veld itself serves for its run. */
  setPolicy?(rules: PermissionRule[], trustedOrigins: string[]): Promise<void>;
  onPermissionRequest?(fn: (payload: PermissionPrompt) => void): () => void;
  answerPermission?(requestId: number, verdict: "allow" | "deny"): Promise<void>;
  abandonPermission?(requestId: number): Promise<void>;
  onPermissions?(
    fn: (payload: {
      viewId: string;
      origin: string | null;
      settings: PermissionSetting[];
    }) => void,
  ): () => void;
  permissions?(viewId: string): Promise<void>;
  setPermission?(
    viewId: string,
    origin: string,
    permission: PermissionId,
    verdict: PermissionVerdict | "default",
  ): Promise<void>;
}

/** A page asking for something nothing has answered yet. */
export interface PermissionPrompt {
  requestId: number;
  viewId: string;
  profile: BrowserProfile;
  /** One request can cover two switches — Electron reports camera and microphone
   *  together — and the answer applies to all of them. */
  permissions: PermissionId[];
  origin: string;
  /** False when a cross-origin subframe asked, which is a different sentence from
   *  the page asking. */
  isMainFrame: boolean;
  paneUrl: string;
}

export type PermissionVerdict = "allow" | "deny";

/** One row of the per-site panel. `source` is what lets a row say where its
 *  answer came from instead of presenting a repo's decision as the user's own. */
export interface PermissionSetting {
  id: PermissionId;
  verdict: PermissionVerdict | "ask";
  source: "user" | "config" | "default";
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
   * The element the pane reparents: the whole content area of the pane.
   *
   * Measured, never painted into. Under Electron it is empty apart from
   * [`View.frame`] — the native view is positioned to match that child's box,
   * which is also why this must be a real laid-out element rather than the React
   * slot (that one is recreated on every remount). While a device is emulated it
   * carries the backdrop the screen sits on.
   */
  container: HTMLDivElement;
  /**
   * The emulated screen's own box inside the container: centred, inset, rounded to
   * the device's shape, and the thing the native view's bounds mirror.
   *
   * Always present, even with no emulation, where it simply fills the container —
   * so there is exactly one element that means "where the page is", rather than a
   * second geometry path that only exists in one of the two modes. It is also what
   * the freeze still is painted onto, so the still lands on the screen rather than
   * across the backdrop.
   */
  frame: HTMLDivElement;
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
   * Whether *this pane's* own DOM overlay is open, which hides only this view.
   *
   * Separate from [`suspendDepth`], which is global because a portalled Mantine
   * overlay can land anywhere on screen and nothing knows which pane it covers. An
   * address bar's suggestion panel is the opposite: it covers exactly one pane, and
   * freezing every *other* browser pane for as long as somebody is typing an address
   * would stop a page they are watching in a second dock.
   */
  overlay: boolean;
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
  /** The media features this view overrides, live copy of `PaneTab.media`. */
  media: PaneMedia | null;
  zoom: number;
  /** The factor and corner radius last pushed with the bounds, so a resize that
   *  changes neither costs no re-emulation — which relayouts the guest page. */
  scale: number;
  radius: number;
  /** The screen's box as last *painted*, in the numbers it was computed from — so a
   *  redraw can tell a real geometry change from the same layout recomputed. Never
   *  compare the frame's inline styles for this: Chromium round-trips lengths through
   *  ~6 significant digits, so the strings differ when the numbers do not. */
  painted: { x: number; y: number; width: number; height: number } | null;
  /** The size a drag is currently at, and the frame it will be applied on.
   *  Coalesced because a mouse produces several moves per painted frame and each
   *  applied size relayouts the user's page. */
  pending: { width: number; height: number } | null;
  pendingFrame: number;
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
        media: v.media,
        zoom: v.zoom,
        background: paneSurface(),
      }))
    ) {
      throw new Error("the desktop shell refused this pane");
    }
    v.shellHasView = true;
    // Forget what the *failed* attempt mirrored. Both caches were written while no
    // shell entry existed, so their sends were dropped: `v.rect` by a geometry sync
    // that went nowhere, `v.visible` by the `applyVisibility` in `mountBrowser`.
    // Leaving them meant the retried view received neither `setBounds` (the rect
    // compares equal, so `pushGeometry` early-returns) nor `setVisible` — a view at
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

/**
 * The theme's content surface, as a hex colour for the shell.
 *
 * A `WebContentsView` paints its own background until the guest does, and Electron's
 * default is white — which in a dark app is a white rectangle flashing on every create,
 * every navigation to a slow page, and at the emulated screen's rounded corners. Read
 * from the same `--term-bg` token the terminal uses, so the two content surfaces agree
 * and neither has to know about the other's theme.
 *
 * Read live rather than cached: it is one `getComputedStyle` on a theme switch and on a
 * view create, and caching it means the first pane after a switch keeps the old one.
 */
function paneSurface(): string {
  if (typeof document === "undefined") return "#ffffff";
  const token = getComputedStyle(document.body).getPropertyValue("--term-bg").trim();
  // Hex only — the shell validates the same way, and a token that has been themed into
  // something exotic should fall back rather than be half-applied.
  return /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/.test(token) ? token : "#ffffff";
}

/**
 * This document's origin — what `isVeldOwnUi` compares against.
 *
 * Read at the call site rather than at module load so the test environment (no
 * `location`) does not need one, and wrapped because `/ide` is served from both
 * the daemon's raw port and the Caddy-fronted name: whichever one the app itself
 * was loaded from is by definition a nested instance of it.
 */
function selfOrigin(): string {
  return typeof location === "undefined" ? "" : location.origin;
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
  // A refused nested `/ide` shows a screen of its own, and there is no view
  // behind it — this has to be here rather than beside the rendering, because
  // this function is the single decision both sides read.
  if (state.nested) return true;
  if (!state.url && !fallbackUrl) return true;
  return !state.loaded && state.loading;
}

/**
 * Whether a DOM overlay is currently standing where this view paints.
 *
 * The two sources are deliberately different shapes — a global counter for
 * portalled overlays nobody can attribute to a pane, a per-view flag for a pane's
 * own — and every place that used to test `suspendDepth` directly reads this
 * instead, so a still cannot be cleared out from under one of them.
 */
function obscured(v: View): boolean {
  return suspendDepth > 0 || v.overlay;
}

/** Whether a mounted view should currently be on screen. A resize drag no longer
 *  hides it — the shell forwards the pointer instead, so the page can reflow under
 *  the cursor (see [`setBrowserResizing`]). */
function shouldShow(v: View): boolean {
  return v.mounted && !obscured(v) && !covered(v);
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
    // `v.visible`, not `!covered(v)`: a view this pane already froze for its own
    // overlay is hidden with a still already painted, and capturing a hidden view
    // would replace that still with whatever the shell answers for one.
    if (desktop && v.mounted && v.visible && !covered(v)) void freezeThenHide(v);
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
 * Hide **one** view behind a DOM overlay the pane itself renders.
 *
 * The address bar's suggestion panel. It used to sit in flow between the chrome and
 * the slot, because a native view paints over DOM and a dropdown positioned over the
 * page is invisible in the desktop app — so typing an address shoved the page down by
 * up to 60% of the pane and reflowed it on every keystroke. Freezing this one view
 * instead puts the panel over a still of the page, dimmed, which is what an address
 * bar looks like everywhere else.
 *
 * A boolean rather than a counter: a pane has one such overlay, and the state that
 * opens it is a single React value, so a count could only ever go wrong. Idempotent,
 * because it is driven from an effect that re-runs on unrelated renders.
 */
export function setBrowserOverlay(id: string, open: boolean): void {
  const v = views.get(id);
  if (!v || v.overlay === open) return;
  v.overlay = open;
  if (open && desktop && v.mounted && v.visible && !covered(v)) {
    void freezeThenHide(v);
    return;
  }
  applyVisibility(v);
  if (!open) thaw(v);
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
  if (image && obscured(v)) {
    await Promise.race([decoded(image), deadline]);
    if (v.freezeGeneration !== generation || !obscured(v)) return;
    v.frame.style.backgroundImage = `url("${image}")`;
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
  if (!v.frame.style.backgroundImage) return;
  const generation = v.freezeGeneration;
  window.setTimeout(() => {
    if (v.freezeGeneration !== generation || obscured(v)) return;
    v.frame.style.backgroundImage = "";
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
      nested: null,
      profile: "default",
      loaded: false,
      emulationScale: 1,
      deviceX: 0,
      deviceY: 0,
      deviceWidth: 0,
      deviceHeight: 0,
      resizing: false,
      touchActive: false,
      mediaActive: false,
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
  media?: PaneMedia | null;
  zoom?: number;
}

function ensure(id: string, options: BrowserViewOptions): View {
  const { profile } = options;
  // A layout can carry a URL from before this guard existed, or from a pane that
  // was forced through it, so the restore path is checked exactly like a
  // navigation — before any view is created for it.
  const url = options.url;
  const nested = url && isVeldOwnUi(url, selfOrigin()) ? url : null;
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
  // The screen, inside the pane's content area. Present in both modes and with or
  // without a device, so "where the page is" is one element and one calculation.
  const frame = document.createElement("div");
  frame.className = "browser-device-frame";
  container.appendChild(frame);

  const v: View = {
    id,
    profile,
    container,
    frame,
    iframe: null,
    state: {
      url: url ?? "",
      title: "",
      loading: Boolean(url) && !nested,
      canGoBack: false,
      canGoForward: false,
      error: null,
      nested,
      profile,
      loaded: false,
      emulationScale: 1,
      deviceX: 0,
      deviceY: 0,
      deviceWidth: 0,
      deviceHeight: 0,
      resizing: false,
      touchActive: false,
      mediaActive: false,
      devToolsOpen: false,
    },
    listeners: new Set(),
    observer: null,
    mounted: false,
    rect: null,
    shellHasView: !desktop,
    freezeGeneration: 0,
    visible: true,
    overlay: false,
    emulation: options.emulation ?? null,
    media: options.media ?? null,
    zoom: clampZoom(options.zoom ?? DEFAULT_ZOOM),
    scale: 1,
    radius: 0,
    painted: null,
    pending: null,
    pendingFrame: 0,
  };
  views.set(id, v);
  const waiting = pending.get(id);
  if (waiting) {
    for (const fn of waiting) v.listeners.add(fn);
    pending.delete(id);
  }

  if (desktop) {
    // Not created at all while refused: making the thing being refused, in order
    // to keep it hidden, would mint the session ids this guard exists to prevent.
    // A forced navigation creates it then, through the same path a failed create
    // is retried by.
    if (!nested) void createShellView(v, url);
  } else {
    const iframe = document.createElement("iframe");
    iframe.className = "browser-frame";
    // No `sandbox`: the pane previews the user's *own* application, and a
    // sandbox without allow-same-origin gives it an opaque origin — no cookies,
    // no localStorage, no logged-in session, which is the whole point of the
    // pane. The frame is cross-origin to /ide either way, which is what keeps it
    // out of this document.
    // No `allow`: `clipboard-write` is `self` by default, and delegating it to
    // arbitrary previewed content lets one click overwrite the user's clipboard.
    // The Electron backend denies every permission, so granting one here would be
    // the browser build being *less* careful than the desktop one.
    iframe.addEventListener("load", () => {
      // `reloadBrowser` bounces this frame through `about:blank`, which fires the
      // same event — treating that as the page having arrived cleared the loading
      // indicator one navigation early.
      if (iframe.src === "about:blank") return;
      patch(v, { loading: false, loaded: true });
    });
    // The element is created either way — a forced navigation assigns `src` to
    // it, and an iframe with no `src` loads nothing.
    if (url && !nested) iframe.src = url;
    // Inside the device frame, not the container: the frame is the screen, and it
    // is what clips the page to the device's rounded corners.
    frame.appendChild(iframe);
    v.iframe = iframe;
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
  // An overlay cannot outlive the pane that renders it. The pane's own effect clears
  // this too; the belt matters because a flag left set on a detached view would keep
  // it hidden for good the next time the tab is opened.
  v.overlay = false;
  applyVisibility(v);
  v.container.remove();
}

/**
 * Point a pane at a URL. Returns the normalized target, or `null` if the input
 * was not an address — the caller records the target in the layout.
 *
 * `force` is the way past the nested-`/ide` guard, and the returned target is the
 * same either way: a refused URL is still where the user asked to go, so the
 * address bar and the layout keep it exactly as they do for a page that failed to
 * load.
 */
export function navigateBrowser(
  id: string,
  raw: string,
  opts: { force?: boolean } = {},
): string | null {
  const v = views.get(id);
  if (!v) return null;
  const url = normalizeBrowserUrl(raw);
  if (!url) {
    patch(v, { error: localError(`Not an http(s) address: ${raw.trim()}`) });
    return null;
  }
  if (!opts.force && isVeldOwnUi(url, selfOrigin())) {
    patch(v, { url, nested: url, loading: false, error: null });
    applyVisibility(v);
    return url;
  }
  patch(v, { url, loading: true, error: null, nested: null });
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
  // A refused pane has nothing to reload, and the retry path below would create
  // the view for the URL that was refused — the one thing this must not do.
  if (v.state.nested) return;
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
  // The screen's box is a function of the emulation, so changing one has to redraw
  // the other — and *this* side owns that box now. Without it the frame kept the
  // previous device's size until something else happened to trigger a sync: the
  // 400 ms tick under Electron, and under the iframe backend nothing at all, where
  // picking a device would appear to do nothing until you resized the pane.
  scheduleGeometrySync();
  if (!desktop) {
    syncGeometry(v);
    return;
  }
  // Nothing to talk to yet; `createShellView` sends it with the create.
  if (!v.shellHasView) return;
  // Surfaced rather than swallowed: the pane's chrome has already changed to say
  // it is emulating a phone, so a refused call has to be visible or the chrome is
  // lying about what the page is being shown as.
  void desktop.emulate(id, emulation).catch(reportFailure(v));
}

/**
 * Override this pane's media features, or clear them with `null`.
 *
 * Same ownership rule as `setBrowserEmulation`: the live copy is written first
 * and unconditionally, so a view the shell has not created yet comes back with
 * the overrides the pane is set to. Electron-only — an `<iframe>` cannot be told
 * what its media queries should answer, and pretending otherwise would put a
 * control in the browser build that silently does nothing.
 */
export function setBrowserMedia(id: string, media: PaneMedia | null): void {
  const v = views.get(id);
  if (!v) return;
  v.media = media;
  if (!desktop || !v.shellHasView) return;
  // Optional-chained for the same version-skew reason as the permission bridge
  // below: an older shell has no `setMedia`, and a media override that silently
  // does nothing is far better than an app that fails to render.
  void desktop.setMedia?.(id, media).catch(reportFailure(v));
}

/**
 * Enter or leave a resize drag.
 *
 * The page stays live and reflows as you drag — which needs one thing arranged
 * first, in each backend, for the same reason: **the thing being resized owns the
 * pointer events that land on it.** A `WebContentsView` is an OS-level sibling, so
 * a mouse event inside its rect belongs to the guest; a cross-origin iframe
 * consumes the ones that land on it too. Either way the drag would lose its moves
 * and never see its `pointerup`, which is a gesture that cannot end.
 *
 * Under Electron the shell forwards the view's own pointer instead
 * (`onBrowserPointer`), which is what lets the view stay visible. The iframe
 * backend has no such channel, so its frame goes pointer-transparent for the
 * gesture — it keeps painting, and it is the thing being resized, so hiding it was
 * never an option there.
 */
export function setBrowserResizing(id: string, resizing: boolean): void {
  // Before the view lookup, deliberately. A pane disposed mid-gesture (a tab closed, a
  // worktree switched, `pruneBrowsers` from an async list change) has already been
  // deleted from `views`, so a disarm behind that guard never ran — and since arming is
  // window-wide, every *sibling* view kept forwarding a cursor read plus an IPC message
  // per mouse move until the page reloaded. The shell resolves this from the window, so
  // it needs nothing of ours to still exist.
  if (desktop) void desktop.drag(resizing).catch(() => {});
  const v = views.get(id);
  if (!v) return;
  patch(v, { resizing });
  if (v.iframe) v.iframe.style.pointerEvents = resizing ? "none" : "";
  if (resizing) return;
  // Drop a frame that has not fired yet, or it would repaint the drag's last size
  // after the applied one has already been drawn.
  if (v.pendingFrame !== 0) cancelAnimationFrame(v.pendingFrame);
  v.pendingFrame = 0;
  v.pending = null;
  // Re-assert the applied emulation, which the drag has been overwriting in the
  // shell. Matters most when the gesture was *cancelled*: the layout still holds the
  // pre-drag device, and without this the view would keep the size the pointer
  // happened to be at when the drag was interrupted.
  if (desktop && v.shellHasView) void desktop.emulate(id, v.emulation).catch(reportFailure(v));
  syncGeometry(v);
}

/** Page zoom for one pane. Stored live for the same recreation reason as the
 *  emulation; under the iframe backend there is no zoom to set (see
 *  [`syncGeometry`]). */
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
 * Two backends, one trigger and one calculation (`deviceLayout`): the emulated
 * screen's box is a function of the pane's, and Electron mirrors that box onto the
 * native view while the browser build lays the iframe out inside it.
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
    for (const view of views.values()) syncGeometry(view);
  });
}

/**
 * Place the emulated screen, then hand the result to whichever backend is live.
 *
 * The single geometry path. `frame` is positioned and shaped here for both
 * backends — the border, the shadow and the rounded corners are DOM even under
 * Electron, because they sit *outside* the native view's rect where DOM is
 * visible; the view itself gets Electron's own `setBorderRadius` so the page is
 * clipped to the same shape rather than squaring off the corners the frame draws.
 */
function syncGeometry(v: View): void {
  if (!v.mounted || !v.container.isConnected) return;
  // A drag owns the frame while it lasts. This runs on a 400 ms tick and on every
  // window resize, and it draws from the *applied* emulation — so during a drag it
  // is the second writer of one element, and it wins whenever the pointer is still:
  // hold the button without moving and the screen snapped back to its pre-drag
  // size, then jumped forward again on the next move. `setBrowserResizing(false)`
  // clears the flag before calling this, so the final size is not skipped.
  if (v.state.resizing) return;
  const box = v.container.getBoundingClientRect();
  if (box.width < 1 || box.height < 1) return;
  const e = v.emulation;

  if (!e) {
    // No device: the page is the pane. The frame fills the container, which keeps
    // one element meaning "where the page is" in both modes.
    v.frame.style.inset = "0";
    // Clear the still **only when the frame was a device-sized screen**, i.e. when this
    // call is the transition out of emulation. Then the still was captured at that
    // smaller size and would stretch across the pane — picking "Pane size" from the open
    // device menu is exactly that path, with views suspended and a still up.
    //
    // Not unconditionally, which is how the first version of this went wrong: this
    // function has no `suspendDepth` guard and runs from a 400 ms tick and from every
    // `ResizeObserver`, so on a pane with *no device at all* it wiped a perfectly good
    // still — one frozen for a splitter drag or a modal elsewhere in the app — within
    // 400 ms, mid-gesture, long before `thaw`'s grace. That is the blank-pane flicker the
    // whole freeze/suspend/thaw mechanism exists to prevent, on the common case.
    if (v.painted !== null && v.frame.style.backgroundImage) {
      v.frame.style.backgroundImage = "";
    }
    v.painted = null;
    v.frame.style.removeProperty("width");
    v.frame.style.removeProperty("height");
    v.frame.style.removeProperty("border-radius");
    delete v.container.dataset.emulated;
    if (v.iframe) resetIframe(v.iframe);
    patch(v, { deviceX: 0, deviceY: 0, deviceWidth: box.width, deviceHeight: box.height });
    pushGeometry(v, { x: box.left, y: box.top, width: box.width, height: box.height }, 1, 0);
    return;
  }

  const { layout, radius } = paintScreen(v, e, box);
  pushGeometry(
    v,
    { x: box.left + layout.x, y: box.top + layout.y, width: layout.width, height: layout.height },
    layout.scale,
    radius,
  );
}

/**
 * Draw the screen at a given emulation: position, size, shape, and the iframe
 * inside it under the browser backend.
 *
 * Split out from [`syncGeometry`] because a resize drag needs exactly this and
 * *not* the part that talks to the shell — the native view is hidden while the
 * pointer is down, so there is nothing to move, and re-emulating per pointer move
 * would relayout the guest page dozens of times. Sharing it is also what makes the
 * dragged screen grow from its centre: `deviceLayout` centres, and there is only
 * one of it.
 */
function paintScreen(
  v: View,
  e: PaneEmulation,
  box: { width: number; height: number },
): { layout: DeviceLayout; radius: number } {
  const layout = deviceLayout(e, box);
  const radius = scaledRadius(e, layout.scale);
  // Container-local, which is the coordinate space the pane's own resize handles
  // are positioned in — the page rect in `syncGeometry` is the same box in window
  // coordinates, which is what a native view's bounds are relative to.
  patch(v, {
    deviceX: layout.x,
    deviceY: layout.y,
    deviceWidth: layout.width,
    deviceHeight: layout.height,
    emulationScale: layout.scale,
  });
  v.container.dataset.emulated = "true";
  // A still is a picture of the screen at its *previous* size, so a geometry change
  // invalidates it — "Fit to pane" keeps its menu open, so views are suspended and a
  // still is up while it changes the layout, and the still would otherwise stay on
  // screen stretched until the thaw 250ms after the menu closed. (The zoom stepper is
  // *not* such a case: page zoom moves no geometry, so `moved` stays false and the pane
  // rightly keeps its still.)
  //
  // Only on an actual change, because this also runs from the 400ms geometry tick, which
  // fires *while* views are suspended and recomputes the layout it already drew.
  // Clearing unconditionally wiped the still within 400ms of every menu opening —
  // defeating the freeze in this feature's own mainline interaction, desktop-only.
  //
  // Compared as **numbers**, against what was last painted. Comparing the inline style
  // strings instead looks equivalent and is not: Chromium round-trips a length through
  // ~6 significant digits, so `533.3333333333334px` reads back as `533.333px` and the
  // test was permanently unequal. Measured across realistic pane sizes, that wiped the
  // still on most of them — the first version of this fix did nothing at all.
  const painted = v.painted;
  const moved =
    painted === null ||
    painted.x !== layout.x ||
    painted.y !== layout.y ||
    painted.width !== layout.width ||
    painted.height !== layout.height;
  if (moved && v.frame.style.backgroundImage) v.frame.style.backgroundImage = "";
  v.painted = { x: layout.x, y: layout.y, width: layout.width, height: layout.height };
  v.frame.style.inset = `${layout.y}px auto auto ${layout.x}px`;
  v.frame.style.width = `${layout.width}px`;
  v.frame.style.height = `${layout.height}px`;
  v.frame.style.borderRadius = `${radius}px`;

  if (v.iframe) {
    // The frame's *own* width is a real viewport, so a page in a 393px iframe sees
    // 393px in its media queries — the layout half of emulation, and most of why
    // anyone reaches for it. `transform` rather than `zoom` because scaling the
    // rendered result is the point: the frame keeps its 393 CSS pixels and takes up
    // fewer of the pane's. What an iframe has no API for is the rest — user agent,
    // touch, device pixel ratio, page zoom — which the device menu states.
    v.iframe.dataset.emulated = "true";
    v.iframe.style.width = `${e.width}px`;
    v.iframe.style.height = `${e.height}px`;
    v.iframe.style.transform = `scale(${layout.scale})`;
    v.iframe.style.transformOrigin = "top left";
  }

  return { layout, radius };
}

/**
 * Resize the screen — for real — to the size a drag is currently at.
 *
 * The page reflows live, so a drag is a responsive test rather than a preview of
 * one: that is the whole reason to have this inside the pane. Centred, because it
 * goes through the same `deviceLayout` as everything else.
 *
 * Coalesced to one frame. Every applied size is an `enableDeviceEmulation`, which
 * relayouts the *user's* page and fires its `resize` handlers — a mouse can produce
 * several moves per frame, and there is no point relayouting an app twice for one
 * painted frame. The emulation is written to the layout only on release.
 */
export function previewBrowserResize(id: string, width: number, height: number): void {
  const v = views.get(id);
  if (!v || !v.emulation) return;
  v.pending = { width, height };
  if (v.pendingFrame !== 0) return;
  v.pendingFrame = requestAnimationFrame(() => {
    v.pendingFrame = 0;
    const size = v.pending;
    if (!size || !v.emulation || !v.mounted || !v.container.isConnected) return;
    const box = v.container.getBoundingClientRect();
    if (box.width < 1 || box.height < 1) return;
    const preview = { ...v.emulation, width: size.width, height: size.height };
    const { layout, radius } = paintScreen(v, preview, box);
    if (!desktop || !v.shellHasView) return;
    // Emulation before bounds: the shell's bounds handler re-applies the metrics
    // when the scale moves, so the other order would apply the *new* scale to the
    // old viewport for one call. Both land in the same tick, so nothing paints in
    // between either way — it is the ordering that is correct, not the timing.
    void desktop.emulate(id, preview).catch(reportFailure(v));
    pushGeometry(
      v,
      { x: box.left + layout.x, y: box.top + layout.y, width: layout.width, height: layout.height },
      layout.scale,
      radius,
    );
  });
}

/**
 * Mouse events the *pane's page* saw, forwarded by the shell during a drag.
 *
 * The gesture's second source of pointer data, and the thing that makes a live
 * resize possible at all: a `WebContentsView` owns every mouse event inside its own
 * rect, so a drag whose pointer crosses the page would otherwise lose its moves and
 * never see its `mouseup`. Coordinates are already in this window's CSS pixels (the
 * shell derives them from the cursor position, not from the event), so a consumer
 * can treat them exactly like a `pointermove`.
 */
export function onBrowserPointer(
  fn: (event: { viewId: string; type: string; x: number; y: number }) => void,
): () => void {
  return desktop ? desktop.onPointer(fn) : () => {};
}

/** Back to the stylesheet's own 100%/100%, rather than to a computed size: the
 *  pane can be resized while no device is set, and a leftover pixel width would
 *  not follow it. */
function resetIframe(frame: HTMLIFrameElement): void {
  delete frame.dataset.emulated;
  frame.style.removeProperty("width");
  frame.style.removeProperty("height");
  frame.style.removeProperty("transform");
  frame.style.removeProperty("transform-origin");
}

/**
 * Report the screen's box to whatever needs it: the pane's own state, and — under
 * Electron — the native view.
 *
 * `scale` and `radius` ride along with the rect because they are one calculation
 * and one moment: a resize changes all three, and applying them in separate round
 * trips shows as a frame of a device at the wrong size or shape.
 */
function pushGeometry(
  v: View,
  box: { x: number; y: number; width: number; height: number },
  scale: number,
  radius: number,
): void {
  const rect = {
    x: Math.round(box.x),
    y: Math.round(box.y),
    width: Math.round(box.width),
    height: Math.round(box.height),
  };
  if (rect.width < 1 || rect.height < 1) return;
  // The pane renders the scale ("1440 × 900 · 42%"), and it is the renderer's
  // number now — the shell used to derive it from the native bounds, which put one
  // calculation in two places.
  patch(v, { emulationScale: scale });
  if (!desktop) return;
  const last = v.rect;
  if (
    last &&
    last.x === rect.x &&
    last.y === rect.y &&
    last.width === rect.width &&
    last.height === rect.height &&
    v.scale === scale &&
    v.radius === radius
  ) {
    return;
  }
  v.rect = rect;
  v.scale = scale;
  v.radius = radius;
  // Forget the cache if the send failed, or the poll below can never re-try it:
  // the rect is recorded *before* the send, and this early-returns while the
  // cached value matches — so a pane sitting still would keep a view at stale
  // bounds forever. (`applyVisibility` writes before sending too, but its value
  // flips on every suspend and resume, so it self-corrects.)
  void desktop.setBounds(v.id, rect, scale, radius).catch(() => {
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
 * happens when it actually differs (`pushGeometry`), which makes the idle cost one
 * `getBoundingClientRect` per visible pane per interval.
 */
if (desktop) {
  window.setInterval(() => {
    for (const v of views.values()) syncGeometry(v);
  }, 400);
  window.addEventListener("resize", () => {
    for (const v of views.values()) syncGeometry(v);
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
    // What the pane cannot know for itself: whether touch survived (something
    // else can hold Chromium's debugger) and whether the inspector is open. Read
    // only when present, so an event about something else cannot reset them. The
    // scale is *not* here — it is this side's own number, pushed with the bounds.
    if (typeof payload.touchActive === "boolean") next.touchActive = payload.touchActive;
    if (typeof payload.mediaActive === "boolean") next.mediaActive = payload.mediaActive;
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
 * Repaint every native view when the app's theme changes.
 *
 * The theme lives on `body[data-theme]`, which is the fact itself rather than a second
 * signal that could drift from it. Without this a view keeps the surface it was created
 * with, so switching to dark leaves every already-open pane flashing white on its next
 * navigation.
 */
if (desktop && typeof document !== "undefined") {
  new MutationObserver(() => {
    void desktop.setBackground(paneSurface()).catch(() => {});
  }).observe(document.body, { attributes: true, attributeFilter: ["data-theme"] });
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

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------
//
// All Electron-only, and every function here is a no-op in the browser build.
// That is not a gap being papered over: an `<iframe>` fallback has no permission
// surface to govern in the first place — the page inside it is subject to *this*
// document's permissions policy, which is the browser's own business and not
// veld's to answer.
//
// **Every call below is optional-chained through the method, not just through
// `desktop`.** The two halves of Veld Desktop update independently — `veld update`
// bumps the daemon (and with it this bundle), while the app updates on its own
// cadence, which `desktop/ARCHITECTURE.md` names as *the* compatibility surface
// and deliberately reports rather than blocks on. So a newer bundle routinely runs
// inside an older shell, where `veldDesktop.browser` exists but these methods do
// not. `desktop?.setPolicy(...)` guards the wrong thing: the bridge is present and
// the *method* is `undefined`, so it throws — synchronously, inside a React effect
// that runs on every `/ide` load, and there is no error boundary under `ui/src`.
// The whole app unmounts to a blank window, not just the pane. A missing method
// has to degrade to "panes keep asking", which is the safe direction anyway.

/**
 * Hand the window's panes the policy they are answered against.
 *
 * Called whenever the selected worktree or its run's URLs change, because both
 * halves come from there: the rules are that checkout's `ide.permissions`, and the
 * trusted origins are the URLs veld serves for its run — the only origins screen
 * capture is granted at without asking.
 */
export function setBrowserPolicy(rules: PermissionRule[], trustedOrigins: string[]): void {
  if (!desktop) return;
  void desktop.setPolicy?.(rules, trustedOrigins).catch(() => {
    // A policy that failed to land means panes keep asking rather than granting
    // silently — the safe direction, and not worth a toast.
  });
}

/**
 * The origin of a URL, or `""` when it has none.
 *
 * Used to decide whether a navigation changed the *site* — a question `state.url`
 * cannot answer, because an in-page navigation changes the URL without leaving
 * the origin.
 */
export function originOf(url: string): string {
  if (!url) return "";
  try {
    return new URL(url).origin;
  } catch {
    return "";
  }
}

export function onPermissionRequest(fn: (prompt: PermissionPrompt) => void): () => void {
  return desktop?.onPermissionRequest ? desktop.onPermissionRequest(fn) : () => {};
}

export function answerPermission(requestId: number, verdict: PermissionVerdict): void {
  void desktop?.answerPermission?.(requestId, verdict).catch(() => {});
}

/**
 * Drop a prompt the UI cannot show, without answering it.
 *
 * The page is refused *this* request and nothing is recorded. Not the same as
 * sending a deny: a stored deny outranks the project config permanently, so
 * answering on the user's behalf because two requests arrived at once would
 * silently block a permission they never saw.
 */
export function abandonPermission(requestId: number): void {
  void desktop?.abandonPermission?.(requestId).catch(() => {});
}

export function onPermissionSettings(
  fn: (payload: { viewId: string; origin: string | null; settings: PermissionSetting[] }) => void,
): () => void {
  return desktop?.onPermissions ? desktop.onPermissions(fn) : () => {};
}

/** Ask for the current site's state — a panel opened before any navigation has none. */
export function requestPermissionSettings(viewId: string): void {
  void desktop?.permissions?.(viewId).catch(() => {});
}

export function setPermission(
  viewId: string,
  origin: string,
  permission: PermissionId,
  verdict: PermissionVerdict | "default",
): void {
  void desktop?.setPermission?.(viewId, origin, permission, verdict).catch(() => {});
}
