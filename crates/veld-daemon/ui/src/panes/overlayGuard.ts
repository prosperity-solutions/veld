/**
 * Keep DOM overlays visible over embedded browser panes.
 *
 * A `WebContentsView` is a native sibling of the page, not a DOM node: it paints
 * over every menu, dialog and dropdown no matter what z-index they carry. There
 * is no CSS answer — the view has to be *hidden* while an overlay that would
 * land on it is open (`pushBrowserSuspend` / `popBrowserSuspend`).
 *
 * Doing that by hand at every overlay call site would be a permanent tax on
 * anyone adding a menu, and the failure is silent: a dropdown that renders
 * behind a pane looks like it never opened. So this watches for them instead.
 * Every Mantine overlay — Modal, Menu.Dropdown, Popover, Select,
 * mantine-contextmenu — mounts through `Portal`.
 *
 * **`Portal` reuses one shared node** (`reuseTargetNode` defaults to true): a
 * single `div[data-mantine-shared-portal-node]` is appended to `body` the first
 * time *any* overlay opens and then never removed, with every later overlay
 * rendering inside it. So watching `body`'s children sees one mutation, ever,
 * and then goes deaf — which is exactly the bug this shipped with. The watch has
 * to be on that node's **subtree**, with `body`'s children watched only to notice
 * it being created. That is also what keeps this cheap: the shared node contains
 * nothing but overlays, so a subtree observer there costs nothing, while one on
 * `body` would fire on every xterm frame.
 *
 * Surfaces this app builds itself are *not* pattern-matched — they call
 * `pushBrowserSuspend`/`popBrowserSuspend` directly from the state that opens
 * them (the ⌘K palette does). This guard is for the framework's portals, whose
 * lifecycle the app never sees.
 *
 * **This is a heuristic, and deliberately the safe kind.** It matches ARIA roles
 * first and class names second; if a future Mantine renames things the miss
 * shows up as a dropdown drawn behind a pane — visible, annoying, not a
 * correctness problem. The opposite failure (suspending too eagerly) blanks a
 * pane for no reason, which is worse, so tooltips are deliberately not matched.
 */

import { popBrowserSuspend, pushBrowserSuspend } from "./browserHost";

/**
 * What counts as an overlay.
 *
 * - `[role="dialog"]` / `[aria-modal="true"]` — Modal, Drawer. The *content*
 *   carries the role and is only in the DOM while open, which is what makes it a
 *   reliable signal. Matching `mantine-Modal-root` by class is not: that wrapper
 *   is left mounted at `display: block` when the modal is closed, so it reads as
 *   an open overlay forever — which is precisely how this shipped hiding every
 *   pane permanently.
 * - `[role="menu"]` — Menu.Dropdown.
 * - `[class*="-dropdown"]` — Popover/Select/Combobox dropdowns, which carry no
 *   role of their own.
 * - `.mantine-contextmenu` — mantine-contextmenu's paper.
 * - `[data-veld-overlay]` — an opt-in for anything hand-built that does get
 *   portalled.
 *
 * Not matched: tooltips and notifications. They are transient and mostly
 * pointer-triggered, so suspending on them would flicker a pane on every hover
 * — and nobody needs to read *through* a browser pane to a tooltip.
 * `role="listbox"` is left out on purpose too: the worktree rail is slated to
 * become one (#169), which would suspend every pane permanently.
 */
const OVERLAY_SELECTOR = [
  '[role="dialog"]',
  '[aria-modal="true"]',
  '[role="menu"]',
  '[class*="-dropdown"]',
  ".mantine-contextmenu",
  "[data-veld-overlay]",
].join(",");

/** Mantine's one shared portal container — see the module comment. */
const SHARED_PORTAL_SELECTOR = "[data-mantine-shared-portal-node]";

/**
 * The bit of an element this rule needs.
 *
 * Structural rather than `Element` so the rule is testable: the UI's test runner
 * is `environment: "node"` and has no DOM at all (#167 batch 5 tracks fixing
 * that). A regression here is invisible in the browser build — z-index works
 * there — and only shows up in Electron, which is exactly the kind of rule that
 * needs a test more than most.
 */
export interface OverlayCandidate {
  matches(selector: string): boolean;
  querySelectorAll(selector: string): ArrayLike<OverlayCandidate>;
  checkVisibility?(options?: { visibilityProperty?: boolean }): boolean;
  getClientRects(): { length: number };
}

/**
 * Whether an element is actually being painted.
 *
 * Presence in the DOM is **not** enough. Mantine's `Combobox` keeps its dropdown
 * mounted and hides it with `display: none`, so a closed `Select` looks exactly
 * like an open one to a selector; and `.mantine-contextmenu` mounts at
 * `visibility: hidden` and reveals itself with a later class change.
 */
function isRendered(el: OverlayCandidate): boolean {
  if (typeof el.checkVisibility === "function") {
    // Covers display, content-visibility and — asked for explicitly — the
    // `visibility` property. Not `opacity`: a modal fading in is painted, and
    // treating it as absent would flash a pane over it for the transition.
    return el.checkVisibility({ visibilityProperty: true });
  }
  return el.getClientRects().length > 0;
}

/** Whether any direct child of `root` is, or contains, a *rendered* overlay. */
export function hasOverlay(root: { children: ArrayLike<OverlayCandidate> }): boolean {
  for (const child of Array.from(root.children)) {
    if (child.matches(OVERLAY_SELECTOR) && isRendered(child)) return true;
    // All matches, not the first: a kept-mounted hidden dropdown must not mask a
    // sibling that really is open.
    for (const el of Array.from(child.querySelectorAll(OVERLAY_SELECTOR))) {
      if (isRendered(el)) return true;
    }
  }
  return false;
}

/**
 * Start watching. Returns a stop function.
 *
 * The observer holds at most one suspend of its own and re-derives the answer
 * from the document after every mutation, rather than pairing each appearance
 * with a disappearance. Nesting (a Select inside a Modal) then needs no special
 * case: two portals are present, the answer is still "yes".
 */
export function watchOverlays(): () => void {
  let suspended = false;
  let scheduled = 0;
  let stopped = false;
  let watchedPortal: Element | null = null;

  const observer = new MutationObserver(() => schedule());

  /** Start watching Mantine's shared portal node once it exists. */
  const watchSharedPortal = () => {
    const node = document.body.querySelector(SHARED_PORTAL_SELECTOR);
    if (!node || node === watchedPortal) return;
    watchedPortal = node;
    // Attributes as well as children: an overlay that is kept mounted opens by
    // flipping `display` (Combobox) or adding a class (mantine-contextmenu), and
    // produces no childList mutation at all. Cheap here — this node holds
    // nothing but overlays.
    observer.observe(node, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["class", "style", "role", "aria-modal"],
    });
  };

  const evaluate = () => {
    scheduled = 0;
    // A frame still queued when `stop()` ran would otherwise re-arm the observer
    // through `watchSharedPortal`, and could take a suspend that nothing will ever
    // pop — hiding every native view for the life of the page. StrictMode's
    // synchronous remount cannot land in that window; an HMR remount of `App`
    // while a Mantine portal mutates can.
    if (stopped) return;
    watchSharedPortal();
    const open = hasOverlay(document.body);
    if (open === suspended) return;
    suspended = open;
    if (open) pushBrowserSuspend();
    else popBrowserSuspend();
  };

  // Coalesced to a frame: opening one dropdown is several mutations, and
  // re-querying on each is wasted work.
  const schedule = () => {
    if (scheduled || stopped) return;
    scheduled = requestAnimationFrame(evaluate);
  };

  // `body`'s children: enough to notice the shared portal node appearing (and a
  // non-reusing `Portal`, which appends and removes its own container).
  observer.observe(document.body, { childList: true });
  evaluate();

  return () => {
    stopped = true;
    if (scheduled) cancelAnimationFrame(scheduled);
    scheduled = 0;
    observer.disconnect();
    watchedPortal = null;
    if (suspended) {
      suspended = false;
      popBrowserSuspend();
    }
  };
}
