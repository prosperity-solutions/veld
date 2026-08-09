/**
 * The pane/tab model for IDE mode.
 *
 * Worktree mode used to be a fixed two-column layout: a terminal placeholder
 * at a hardcoded 46% next to the service launcher. That does not survive the
 * next two increments — the embedded browser (#167 batch 3) and run
 * diagnostics (batch 4) both want that same region, and each one bolted on as
 * another fixed column makes the window unusable. So the region is a **dock**
 * instead: two side-by-side tab hosts with a draggable split, and a tab kind
 * per content type. Adding the browser later is a new `PaneKind` and a
 * renderer, not a layout rewrite.
 *
 * Everything here is pure so the layout rules are testable without a DOM;
 * the React side owns only the state cell and the rendering.
 */

import type { PaneLaunchMode } from "../api";
import {
  type PaneEmulation,
  type PaneMedia,
  sanitizeEmulation,
  sanitizeMedia,
  sanitizeZoom,
} from "./devices";

/**
 * What a tab shows.
 *
 * **One source of truth**: the runtime array, because a restored layout has to
 * be validated against the same set (`parseTab` below). A second hardcoded list
 * there would let a new kind work perfectly until the first reload, then vanish
 * silently — `parseLayouts` discards anything it doesn't recognise by design.
 *
 * `new` is a pane that has not decided yet: the `+` button opens one, and picking
 * a kind inside it *replaces* the tab in place. That way the choice happens where
 * the content will be, at content size, instead of in a menu the size of a
 * cursor — and an empty dock and a fresh tab are then the same screen.
 *
 * `logs` and `nodes` are the run's diagnostics: the log viewer and the per-node
 * health/stats/actions table, both scoped to the selected worktree's run and both
 * shared with runs mode (`ui/src/shared/`). They are kinds — unlike the URLs
 * below — because they are content you sit in front of and arrange beside a
 * terminal, not a way to get somewhere else.
 *
 * **Adding a kind that owns live state?** A terminal is the worked example, and it
 * needs three things beyond a renderer: a module-level registry outside React (so
 * a tab switch cannot destroy it — `panes/terminalHost.ts`), a durable home for
 * the ids naming that state (`layoutSlotKey` below, because a Veld Desktop restart
 * empties `sessionStorage`), and a way to notice that the state it named is *gone*
 * (`EXPECTED_RESUMES` in `terminalHost.ts`, which must read through `loadLayouts`
 * or it is empty in exactly the case that matters). `panes/browserHost.ts` is the
 * second example and needs none of the durability, because a page is re-creatable.
 *
 * There is deliberately no `services` kind. The run's URLs are a *launcher*, not
 * a peer of a terminal and a page, and a launcher belongs wherever you are about
 * to need it: a `new` pane and a browser pane with no URL both show them
 * (`panes/VeldLinks.tsx`). Having a kind for it meant a singleton tab id, a
 * "does it already exist" check at every call site that could open one, and a
 * second place to render the same rows.
 */
export const PANE_KINDS = ["terminal", "browser", "logs", "nodes", "new"] as const;

export type PaneKind = (typeof PANE_KINDS)[number];

function isPaneKind(v: unknown): v is PaneKind {
  return typeof v === "string" && (PANE_KINDS as readonly string[]).includes(v);
}

/**
 * Cookie jars a browser pane can run in.
 *
 * The *allowed* set, not the menu: the name becomes an Electron session
 * partition, so it is an identifier the main process has to validate anyway
 * (`PROFILE_RE` in `desktop/src/browserViews.js`), and a restored layout has to
 * be checked against the same list. Which slots actually **exist** is a set the
 * user builds up — see [`SESSIONS_STORAGE_KEY`].
 *
 * `default` is what a pane gets when nobody chose; it stays uncoloured so the
 * common case has no marker to read. Eight slots above it, because that is how
 * many colours stay tellable apart at the size of a tab dot — and the colour is
 * the whole point: it answers "which session is this pane?" without a menu.
 *
 * The slots are **named after animals, not numbered**. A number implies a
 * sequence, so removing "Session 2" and being left with "Default, Session 3"
 * reads as something broken rather than as a set with one item taken out.
 * A name has no successor to be missing. (The worktree rail's emoji set is the
 * same idea, for the same reason.) The name is also the Electron partition
 * (`persist:veld-browser-otter`), so the identifier says what it is.
 *
 * *Naming* a slot is what would need persistence both Veld Desktop and a browser
 * tab agree on, which is the settings store in #167 batch 5 — deliberately not
 * invented here. Clearing a session's data is offered per slot in the pane's
 * session menu.
 */
export const BROWSER_PROFILES = [
  "default",
  "otter",
  "wombat",
  "gecko",
  "badger",
  "puffin",
  "lemur",
  "quokka",
  "narwhal",
] as const;

export type BrowserProfile = (typeof BROWSER_PROFILES)[number];

/** How many sessions can exist alongside the default one. */
export const MAX_EXTRA_SESSIONS = BROWSER_PROFILES.length - 1;

/**
 * A slot's colour, or `null` for the default one.
 *
 * Literal hexes, not theme tokens: these are identity markers that must mean the
 * same thing in both themes — a session that changes colour when you switch
 * theme identifies nothing. The hues are eight that stay distinct at dot size,
 * mostly borrowed from the terminal's ANSI palette so the app has one set.
 */
export const BROWSER_PROFILE_COLORS: Record<BrowserProfile, string | null> = {
  default: null,
  otter: "#5aa2e0",
  wombat: "#e6b43c",
  gecko: "#3fbf7f",
  badger: "#b98ce0",
  puffin: "#4fbfc0",
  lemur: "#f2792b",
  quokka: "#ec6fa9",
  narwhal: "#e05a50",
};

/** Display name for a slot: "Default", "Otter", "Wombat"… */
export function browserProfileLabel(profile: BrowserProfile): string {
  return profile.charAt(0).toUpperCase() + profile.slice(1);
}

function isBrowserProfile(v: unknown): v is BrowserProfile {
  return typeof v === "string" && (BROWSER_PROFILES as readonly string[]).includes(v);
}

export interface PaneTab {
  /** Stable across re-renders and worktree switches — it keys the live
   *  terminal in `terminalHost` (so reusing an id reuses a shell) and the live
   *  view in `browserHost`. */
  id: string;
  kind: PaneKind;
  title: string;
  /**
   * `terminal` only: the `ide.panes[].id` this pane was created from, for a
   * config-declared pane. Absent for an ordinary terminal, which runs a login
   * shell.
   *
   * **A name, and nothing else.** The command and the session token live in the
   * daemon (`pane_sessions`), which is what makes this field cheap to lose: a
   * dropped `spec` degrades the pane to a plain terminal, where a dropped token
   * would have stranded a conversation. The detach path carries it without
   * knowing about it — `safeTransferTab` passes unknown fields through on
   * purpose — so this parser is the only gate it has to clear.
   */
  spec?: string;
  /** `browser` only: where the pane opens, and where it returns after a
   *  reload. Kept in the layout rather than in `browserHost` because it is the
   *  one piece of a pane worth restoring — the page itself is re-fetchable. */
  url?: string;
  /** `browser` only; defaults to `default`. */
  profile?: BrowserProfile;
  /**
   * `browser` only: the device this pane emulates, absent when it shows itself
   * at pane size.
   *
   * In the layout for the same reason the URL is, plus a harder one: emulation is
   * per-`WebContents` and a pane switching session **destroys and recreates its
   * view**, so the state has to live somewhere that outlives the view and be
   * re-asserted on create. `browserHost` holds the live copy; this is the record.
   */
  emulation?: PaneEmulation;
  /**
   * `browser` only: the media features this pane overrides for its page
   * (`prefers-color-scheme` and friends), absent when it reports the host's.
   *
   * In the layout for the same reason `emulation` is — it is per-`WebContents`,
   * and a pane switching session destroys and recreates its view.
   */
  media?: PaneMedia;
  /**
   * `browser` only: page zoom factor, absent at 100%.
   *
   * Same recreation problem as `emulation`, and one of its own — Chromium's zoom
   * is per *origin*, so a navigation adopts whatever the origin was last viewed
   * at and the pane's own setting has to be re-asserted over it.
   */
  zoom?: number;
}

export interface Dock {
  tabs: PaneTab[];
  /** `null` only when the dock is empty. */
  activeId: string | null;
}

/** Which of the two docks: 0 is the left/primary. */
export type DockIndex = 0 | 1;

/**
 * What a layout setter accepts.
 *
 * The updater form exists because two panes can report a change in the same
 * commit — two browser panes side by side both finishing a navigation, say. A
 * plain value is computed from the `layout` of the render it was written in, so
 * the second write would silently discard the first.
 */
export type PaneLayoutUpdate = PaneLayout | ((prev: PaneLayout) => PaneLayout);

export interface PaneLayout {
  docks: [Dock, Dock];
  /** Fraction of the width given to the left dock, 0..1. Only meaningful
   *  when both docks have tabs. */
  ratio: number;
  /** Where a newly opened tab goes. */
  focused: DockIndex;
}

/** Bounds on the split, so a drag can't reduce a dock to an unusable sliver
 *  (or to zero, which reads as "my terminal disappeared"). */
export const MIN_RATIO = 0.15;
export const MAX_RATIO = 0.85;
export const DEFAULT_RATIO = 0.5;

/**
 * Keep the one visible dock as the *left* one.
 *
 * With a single pane on screen there is no left or right, so a layout that is
 * empty on the left and full on the right is a distinction with no visible
 * meaning — and it leaks into the UI as a context menu offering "Move to the
 * left pane" when there is nothing to the left of anything. Emptying the left
 * dock therefore slides the right one over.
 *
 * Returns the same object when there is nothing to do, so this is safe to run on
 * every mutation.
 */
function normalizeDocks(layout: PaneLayout): PaneLayout {
  const [left, right] = layout.docks;
  if (left.tabs.length > 0 || right.tabs.length === 0) return layout;
  return { ...layout, docks: [right, { tabs: [], activeId: null }], focused: 0 };
}

export function clampRatio(r: number): number {
  if (!Number.isFinite(r)) return DEFAULT_RATIO;
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, r));
}

/**
 * The layout a worktree starts with: **one undecided pane**, nothing else.
 *
 * It used to open split, with an empty browser pane on the right showing the
 * run's URLs. That was inherited from the fixed two-column layout this dock
 * replaced, and it stopped earning its place once the chooser grew: the *same*
 * URL list is already on the `new` pane, so the split showed it twice, and it
 * imposed a two-column arrangement on every worktree the first time it was
 * opened — including the many that want one full-width terminal. Splitting is a
 * drag or a menu item away; un-splitting something you never asked for is
 * busywork on every new checkout.
 */
export function defaultLayout(ratio = DEFAULT_RATIO): PaneLayout {
  // A `new` pane, not a terminal: selecting a worktree must not start a shell.
  //
  // A terminal tab's id *is* a daemon session id, so seeding one here meant
  // browsing the worktree rail spent the daemon's session budget — one real
  // shell process per worktree merely looked at, against a cap of 48 — and, now
  // that shells outlive the daemon, one holder process each to go with it. The
  // chooser this renders has a Terminal button as its first item, so asking for
  // one is a single click; nothing is hidden, only deferred to the point where
  // the user actually wants a shell.
  const chooser = newPaneTab();
  return {
    docks: [
      { tabs: [chooser], activeId: chooser.id },
      { tabs: [], activeId: null },
    ],
    // Kept even with nothing in the second dock: it is what a later split opens
    // at, and inheriting the width from the last worktree is what makes the
    // ratio feel like a window preference rather than a per-worktree surprise.
    ratio: clampRatio(ratio),
    focused: 0,
  };
}

/**
 * A tab id, which for a terminal tab is also its **daemon session id** — the
 * name the page uses to ask for the same shell back after a reload (see
 * `crates/veld-daemon/src/pty.rs`).
 *
 * So it must be unique for longer than the page: reusing one adopts whatever
 * shell still answers to it. It is an identifier, not a credential — an attach
 * is authorised by the CSRF-gated ticket and the `Origin` allowlist, not by
 * knowing this string — so the non-crypto fallback below is not a weakness,
 * just a less collision-proof name. The daemon accepts `[A-Za-z0-9_-]{1,64}`,
 * and so does the desktop shell for a browser view id.
 */
export function newTabId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  const rand = () => Math.floor(Math.random() * 0xffffffff).toString(16);
  return `t-${Date.now().toString(36)}-${rand()}${rand()}`;
}

export function allTabs(layout: PaneLayout): PaneTab[] {
  return [...layout.docks[0].tabs, ...layout.docks[1].tabs];
}

export function findTab(layout: PaneLayout, id: string): PaneTab | null {
  return allTabs(layout).find((t) => t.id === id) ?? null;
}

export function hasTab(layout: PaneLayout, id: string): boolean {
  return findTab(layout, id) !== null;
}

/** Whether a dock is currently rendered. An empty dock is not shown at all —
 *  the other one takes the full width. */
export function dockVisible(layout: PaneLayout, index: DockIndex): boolean {
  return layout.docks[index].tabs.length > 0;
}

/**
 * Add a tab to a dock and focus it.
 *
 * Adding a tab whose id already exists anywhere just activates the existing
 * one: two tabs with the same id would fight over one live terminal.
 */
export function addTab(layout: PaneLayout, index: DockIndex, tab: PaneTab): PaneLayout {
  if (hasTab(layout, tab.id)) return activateTab(layout, tab.id);
  const docks: [Dock, Dock] = [layout.docks[0], layout.docks[1]];
  docks[index] = { tabs: [...docks[index].tabs, tab], activeId: tab.id };
  return { ...layout, docks, focused: index };
}

/**
 * Add a tab at a position, rather than at the end.
 *
 * `addTab` appends, which is right for a tab this window created. A tab
 * *arriving from another window* has a position: the caret the target was
 * showing while the pointer hovered its strip. Without this, a cross-window drop
 * could only land at the end — the same drop inside one window honours where you
 * aimed, and a gesture that means two different things depending on which window
 * you release over is the confusion this whole model exists to remove.
 */
export function insertTab(
  layout: PaneLayout,
  index: DockIndex,
  tab: PaneTab,
  at?: number,
): PaneLayout {
  if (hasTab(layout, tab.id)) return activateTab(layout, tab.id);
  const docks: [Dock, Dock] = [layout.docks[0], layout.docks[1]];
  const tabs = docks[index].tabs;
  const pos = Math.max(0, Math.min(at ?? tabs.length, tabs.length));
  docks[index] = {
    tabs: [...tabs.slice(0, pos), tab, ...tabs.slice(pos)],
    activeId: tab.id,
  };
  return { ...layout, docks, focused: index };
}

/** Add a tab to whichever dock last had focus. */
export function addTabToFocused(layout: PaneLayout, tab: PaneTab): PaneLayout {
  return addTab(layout, layout.focused, tab);
}

/**
 * Close a tab.
 *
 * The successor is the tab to the right, falling back to the one on the left
 * — the same rule editors use, and the one that keeps the eye where it was.
 * A dock emptied by this keeps `focused` pointing at a dock that still has
 * tabs, so the next "new terminal" doesn't open into a hidden column.
 */
export function closeTab(layout: PaneLayout, id: string): PaneLayout {
  const docks: [Dock, Dock] = [layout.docks[0], layout.docks[1]];
  let touched: DockIndex | null = null;

  for (const i of [0, 1] as DockIndex[]) {
    const at = docks[i].tabs.findIndex((t) => t.id === id);
    if (at < 0) continue;
    touched = i;
    const tabs = docks[i].tabs.filter((t) => t.id !== id);
    let activeId = docks[i].activeId;
    if (activeId === id) {
      const next = tabs[at] ?? tabs[at - 1] ?? null;
      activeId = next ? next.id : null;
    }
    docks[i] = { tabs, activeId };
  }
  if (touched === null) return layout;

  let focused = layout.focused;
  if (docks[focused].tabs.length === 0) {
    const other = (focused === 0 ? 1 : 0) as DockIndex;
    if (docks[other].tabs.length > 0) focused = other;
  }
  return normalizeDocks({ ...layout, docks, focused });
}

/** Make a tab active in its own dock, and focus that dock. */
export function activateTab(layout: PaneLayout, id: string): PaneLayout {
  const docks: [Dock, Dock] = [layout.docks[0], layout.docks[1]];
  let focused = layout.focused;
  for (const i of [0, 1] as DockIndex[]) {
    if (!docks[i].tabs.some((t) => t.id === id)) continue;
    docks[i] = { ...docks[i], activeId: id };
    focused = i;
  }
  return { ...layout, docks, focused };
}

export function dockOf(layout: PaneLayout, id: string): DockIndex | null {
  return (
    ([0, 1] as DockIndex[]).find((i) => layout.docks[i].tabs.some((t) => t.id === id)) ?? null
  );
}

/** Move a tab to the other dock, keeping it active there. A no-op for a tab
 *  that is alone in its dock and would just swap sides. */
export function moveTabToOtherDock(layout: PaneLayout, id: string): PaneLayout {
  const from = dockOf(layout, id);
  if (from === null) return layout;
  if (layout.docks[from].tabs.length === 1) return layout;
  return moveTab(layout, id, (from === 0 ? 1 : 0) as DockIndex);
}

/**
 * Put a tab at `index` in `dock` — the one operation drag-and-drop needs,
 * covering both reordering within a strip and moving across docks.
 *
 * `index` counts positions in the destination **after** the tab has been
 * removed, which is what a drop indicator between two tabs means. Omit it to
 * append. Dropping a tab back exactly where it already was returns the same
 * object, so a stray drag doesn't churn React or reset the dock's focus.
 */
export function moveTab(
  layout: PaneLayout,
  id: string,
  dock: DockIndex,
  index?: number,
): PaneLayout {
  const from = dockOf(layout, id);
  if (from === null) return layout;
  const tab = layout.docks[from].tabs.find((t) => t.id === id);
  if (!tab) return layout;

  const remaining = layout.docks[from].tabs.filter((t) => t.id !== id);
  const target = from === dock ? remaining : layout.docks[dock].tabs;
  const at = Math.max(0, Math.min(index ?? target.length, target.length));

  const before = layout.docks[from].tabs.findIndex((t) => t.id === id);
  if (from === dock && before === at) return layout;

  const docks: [Dock, Dock] = [layout.docks[0], layout.docks[1]];
  const inserted = [...target.slice(0, at), tab, ...target.slice(at)];
  if (from === dock) {
    docks[dock] = { tabs: inserted, activeId: id };
  } else {
    // Leaving a dock: it needs a new active tab (or none, if now empty).
    const wasActive = layout.docks[from].activeId === id;
    docks[from] = {
      tabs: remaining,
      activeId: wasActive
        ? (remaining[before] ?? remaining[before - 1] ?? null)?.id ?? null
        : layout.docks[from].activeId,
    };
    docks[dock] = { tabs: inserted, activeId: id };
  }
  // Dragging the left dock's only tab to the right is a no-op once normalised,
  // which is right: with one pane on screen the sides mean nothing.
  return normalizeDocks({ ...layout, docks, focused: dock });
}

/**
 * Put a tab on one side of the split, creating the second dock if there is only
 * one on screen.
 *
 * This is what dropping a tab on a pane's left or right *edge* means, and it is
 * the half `moveTab` cannot express. With both docks visible the two coincide —
 * "the right-hand side" is dock 1 either way. With one dock they do not: making
 * the dragged tab the *left* pane means everything else has to become the right
 * one, and there is no dock index that says so.
 *
 * A dock's only tab dropped on its own side is a no-op, not an empty pane: with
 * one pane on screen the sides mean nothing, which is the rule `normalizeDocks`
 * already enforces everywhere else.
 */
export function splitWithTab(layout: PaneLayout, id: string, side: DockIndex): PaneLayout {
  const from = dockOf(layout, id);
  if (from === null) return layout;
  if (dockVisible(layout, 0) && dockVisible(layout, 1)) return moveTab(layout, id, side);

  // One dock on screen, which `normalizeDocks` guarantees is dock 0.
  const tab = layout.docks[0].tabs.find((t) => t.id === id);
  if (!tab) return layout;
  const rest = layout.docks[0].tabs.filter((t) => t.id !== id);
  if (rest.length === 0) return layout;
  if (side === 1) return moveTab(layout, id, 1);

  const restActive =
    layout.docks[0].activeId === id
      ? (rest[layout.docks[0].tabs.findIndex((t) => t.id === id)] ?? rest[rest.length - 1]).id
      : layout.docks[0].activeId;
  return normalizeDocks({
    ...layout,
    docks: [
      { tabs: [tab], activeId: tab.id },
      { tabs: rest, activeId: restActive },
    ],
    focused: 0,
  });
}

/**
 * Take in tabs handed back by a detached window that just closed.
 *
 * Appended to the focused dock rather than restored to where they were: the
 * layout has moved on since the detach, and a remembered index is a position in
 * a strip that no longer looks like that. Ids already present are skipped —
 * two tabs with one id would fight over one shell, and the window handing them
 * back has already stopped rendering them either way.
 *
 * `undefined` for a worktree this window has never opened: the tabs are the
 * whole layout then, which is right — they are the only thing known about it.
 */
export function adoptTabs(layout: PaneLayout | undefined, tabs: PaneTab[]): PaneLayout | null {
  const fresh = tabs.filter((t) => !layout || !hasTab(layout, t.id));
  if (fresh.length === 0) return null;
  if (!layout) {
    return {
      docks: [
        { tabs: fresh, activeId: fresh[0].id },
        { tabs: [], activeId: null },
      ],
      ratio: DEFAULT_RATIO,
      focused: 0,
    };
  }
  return fresh.reduce((acc, tab) => addTab(acc, acc.focused, tab), layout);
}

/**
 * Validate a list of tabs arriving from another window.
 *
 * The same gate a restored layout goes through (`parseTab`), for the same
 * reason: this data has been out of the page — through the shell's main process
 * and a second renderer — and a `javascript:` URL or an unbounded user-agent
 * string reaching a view from *here* is no better than one reaching it from
 * storage.
 */
export function parseTransferTabs(value: unknown): PaneTab[] {
  if (!Array.isArray(value)) return [];
  const seen = new Set<string>();
  const out: PaneTab[] = [];
  for (const entry of value) {
    const tab = parseTab(entry);
    if (!tab || seen.has(tab.id)) continue;
    seen.add(tab.id);
    out.push(tab);
  }
  return out;
}

export function setRatio(layout: PaneLayout, ratio: number): PaneLayout {
  return { ...layout, ratio: clampRatio(ratio) };
}

/** Mark a dock as the target for new tabs. Returns the same object when
 *  nothing changes — focus events fire on every click inside a dock, and a
 *  fresh object each time would re-render the whole region for nothing. */
export function focusDock(layout: PaneLayout, index: DockIndex): PaneLayout {
  if (layout.focused === index) return layout;
  if (layout.docks[index].tabs.length === 0) return layout;
  return { ...layout, focused: index };
}

/** The tabs a dock renders, and which one is on top. */
export function activeTab(layout: PaneLayout, index: DockIndex): PaneTab | null {
  const dock = layout.docks[index];
  return dock.tabs.find((t) => t.id === dock.activeId) ?? null;
}

/** Every terminal id in a layout — what `terminalHost` must keep alive, and
 *  by omission what it may dispose. */
export function terminalIds(layout: PaneLayout): string[] {
  return allTabs(layout)
    .filter((t) => t.kind === "terminal")
    .map((t) => t.id);
}

/**
 * The last browser pane with nothing loaded in it, if there is one.
 *
 * *Last*, so repeatedly asking for the run's URLs lands on the same pane rather
 * than cycling through however many blank ones happen to be open. A pane counts as
 * blank by having no URL, which is exactly the condition under which it shows the
 * URL list — so this asks "is one already showing what I want?".
 */
export function lastBlankBrowserId(layout: PaneLayout): string | null {
  const blanks = allTabs(layout).filter((t) => t.kind === "browser" && !t.url);
  return blanks.length > 0 ? blanks[blanks.length - 1].id : null;
}

/** The same, for `browserHost`. */
export function browserIds(layout: PaneLayout): string[] {
  return allTabs(layout)
    .filter((t) => t.kind === "browser")
    .map((t) => t.id);
}

/**
 * Which session slots are occupied — the set of sessions that *exist*.
 *
 * Computed across **every** worktree's layout, not just the visible one: a
 * session is a cookie jar shared by the whole page, so a pane in a worktree you
 * have since switched away from still holds its slot. Scoping this to one layout
 * would offer a "new session" that quietly adopted another pane's jar.
 */
export function sessionsInUse(layouts: Iterable<PaneLayout>): Set<BrowserProfile> {
  const used = new Set<BrowserProfile>();
  for (const layout of layouts) {
    for (const tab of allTabs(layout)) {
      if (tab.kind === "browser") used.add(tab.profile ?? "default");
    }
  }
  return used;
}

/**
 * The lowest slot not in `taken`, or `null` when all of them are.
 *
 * *Lowest*, not next-after-the-highest, so removing session 3 frees that slot
 * and its colour for reuse instead of marching towards the cap.
 */
export function nextFreeProfile(taken: Set<BrowserProfile>): BrowserProfile | null {
  for (const p of BROWSER_PROFILES) {
    if (p !== "default" && !taken.has(p)) return p;
  }
  return null;
}

// ---------------------------------------------------------------------------
// Session sets
// ---------------------------------------------------------------------------

/**
 * Which sessions exist, per worktree.
 *
 * This is an **explicit set**, not something derived from which slots panes
 * currently occupy. Deriving it was the first attempt and it was wrong in the
 * most confusing way possible: moving a pane onto a new session vacated its old
 * slot, so adding a session appeared to delete the previous one. A session is a
 * cookie jar that outlives the pane that made it — you build a set of them up.
 *
 * `localStorage`, not the daemon: a session only means anything under Electron
 * (the browser build's iframe backend has no cookie jars of its own), so there
 * is no second client for the list to disagree with. That is what makes this
 * *not* the settings store #167 batch 5 needs — it is one client's preference
 * about its own capability. It is also `localStorage` rather than
 * `sessionStorage` on purpose: unlike a layout, this names no live resource, so
 * two windows sharing it is correct rather than a conflict.
 *
 * Partitions themselves stay global (`persist:veld-browser-<slot>`), so two
 * worktrees whose sets both hold the same slot share that jar. That keeps the slot
 * name a plain identifier rather than a composite, at a cost worth stating
 * honestly: cookies scoped to the *project* rather than the run are shared (the
 * default template is `{service}.{run}.{project}.localhost`, so only the run
 * differs between worktrees), and a third-party origin — a login provider on its
 * own domain — is shared unconditionally. Keying the partition by worktree as well
 * is the fix if that ever bites.
 */
export const SESSIONS_STORAGE_KEY = "veld.browserSessions.v1";

/**
 * Put a session set in canonical shape: the default slot always present,
 * duplicates dropped, slot order rather than insertion order.
 *
 * Slot order matters for more than tidiness — the menu is read top to bottom and
 * a session's colour is tied to its slot, so a list that reordered itself as
 * sessions came and went would make the colours look arbitrary.
 */
export function normalizeSessionSet(
  slots: Iterable<BrowserProfile>,
  ...extra: Array<Iterable<BrowserProfile>>
): BrowserProfile[] {
  const present = new Set<BrowserProfile>(["default", ...slots]);
  for (const more of extra) for (const p of more) present.add(p);
  return BROWSER_PROFILES.filter((p) => present.has(p));
}

/** Read the stored sets, tolerating anything that isn't the shape we wrote. */
export function parseSessionSets(raw: string | null): Record<number, BrowserProfile[]> {
  if (!raw) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return {};
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {};

  const out: Record<number, BrowserProfile[]> = {};
  for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
    const id = Number(key);
    if (!Number.isInteger(id) || !Array.isArray(value)) continue;
    // Unknown slot names are dropped rather than carried: the name becomes an
    // Electron partition, and the shell would refuse it anyway.
    out[id] = normalizeSessionSet(value.filter(isBrowserProfile));
  }
  return out;
}

export function serializeSessionSets(sets: Record<number, BrowserProfile[]>): string {
  return JSON.stringify(sets);
}

/**
 * A worktree's sessions: what was stored, plus any slot its panes are actually
 * on.
 *
 * The union matters on restore — a layout can name a session the stored set has
 * lost (storage cleared, an older build, a hand-edit), and a pane whose own
 * session is missing from its own menu is worse than an extra entry.
 */
export function sessionSetFor(
  sets: Record<number, BrowserProfile[]>,
  worktreeId: number,
  layout?: PaneLayout,
): BrowserProfile[] {
  return normalizeSessionSet(
    sets[worktreeId] ?? [],
    layout ? sessionsInUse([layout]) : [],
  );
}

/**
 * Accept a URL for a browser pane, or reject it.
 *
 * Only `http(s)`: a pane is a preview of a dev server, and every other scheme
 * a URL parser accepts turns one into something else — `file:` into a local
 * file reader, `javascript:` into script in the pane's own origin. The desktop
 * shell enforces the same rule (`safeUrl` in `desktop/src/browserViews.js`),
 * because a renderer is not a trust boundary; this copy is what gives the user
 * an error in the address bar instead of a silently ignored Enter.
 *
 * A bare `host:port` or `host/path` is completed to `https://` — typing
 * `localhost:3000` into an address bar is the normal way to use one. That is
 * also the reason the scheme test below is not just "is there a colon":
 * `localhost:3000` parses as a URL with the scheme `localhost:`, so a naive
 * check refuses the most common thing anyone types.
 */
export function normalizeBrowserUrl(raw: string): string | null {
  const text = raw.trim();
  if (text === "") return null;

  const explicitScheme = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(text);
  // `host:port[/path]` — a colon followed by digits is a port, not a scheme.
  const hostPort = /^[^\s/?#:]+:\d+(?:[/?#].*)?$/.test(text);
  if (!explicitScheme && !hostPort && /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(text)) {
    // A scheme with no authority: `javascript:`, `data:`, `mailto:`, `about:`.
    return null;
  }

  let u: URL;
  try {
    u = new URL(explicitScheme ? text : `https://${text}`);
  } catch {
    return null;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return null;
  if (u.hostname === "") return null;
  return u.toString();
}

/** `/`, `/ide`, or anything under `/ide` — the two paths that serve this app. */
const VELD_UI_PATH = /^\/(?:ide(?:\/.*)?)?$/;

/**
 * Whether a URL is Veld's own management UI.
 *
 * A browser pane pointed at `/ide` is the first thing anyone tries, and the
 * reason to catch it is not the joke. A nested instance is a second, complete
 * copy of this app against the *same* daemon: it mounts its own pane registry,
 * mints its own PTY session ids against the 48-session cap, and writes the shared
 * worktree layout store and the desktop claim map from a place no window knows
 * about. Two of those are silent and the third fights the outer app for its own
 * shells.
 *
 * Matched on **origin or `veld.localhost`**, and deliberately not on "any
 * loopback host": a pane's whole job is previewing a dev server on
 * `localhost:3000`, and a project with its own `/ide` route is not far-fetched.
 * `veld.localhost` is Veld's by construction — a run's services get
 * `<node>.<run>.veld.localhost`, which is a different hostname — so the pair
 * covers both ways the daemon is reachable (the raw port the app loads from, and
 * the Caddy-fronted name) with nothing else caught.
 *
 * `/` is included alongside `/ide` for two reasons: pointing a pane at Veld's own
 * dashboard is the same non-use as pointing it at `/ide`, and it is the one place
 * a *click* could otherwise reach a nested `/ide` without going through this
 * check. The known gap is a dev instance served on a `VELD_MANAGEMENT_HOST` of
 * its own; the guard offers an explicit way through, which is the answer for both
 * that and any false positive.
 */
export function isVeldOwnUi(url: string, selfOrigin: string): boolean {
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return false;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return false;
  if (!VELD_UI_PATH.test(u.pathname)) return false;
  return u.origin === selfOrigin || u.hostname === "veld.localhost";
}

/** Short, stable label for a browser tab opened without a name of its own. A
 *  hostname stays as it is — that is what a hostname looks like — but the
 *  fallback is a word in a tab strip, so it is capitalised like the others. */
export function urlLabel(url: string | undefined): string {
  if (!url) return "Browser";
  try {
    return new URL(url).host || "Browser";
  } catch {
    return "Browser";
  }
}

/** A new browser tab. `url` is normalised here so no caller can seed a pane
 *  with a scheme the view would refuse. */
export function browserTab(opts: {
  url?: string;
  title?: string;
  profile?: BrowserProfile;
}): PaneTab {
  const url = opts.url ? (normalizeBrowserUrl(opts.url) ?? undefined) : undefined;
  return {
    id: newTabId(),
    kind: "browser",
    title: opts.title || urlLabel(url),
    ...(url ? { url } : {}),
    profile: opts.profile ?? "default",
  };
}

/**
 * Which of a worktree's config panes the daemon holds a resume token for, and
 * **which worktree the answer is about**.
 *
 * The pairing is the point. See [`paneAnswerFor`].
 */
export interface PaneSessionAnswer {
  worktreeId: number;
  resumable: Set<string>;
}

/**
 * The answer, but only if it is about the worktree being rendered.
 *
 * A pane commits its start plan once, at first mount, and needs to know whether
 * it has a token. That has been got wrong twice, in the same place, for the same
 * underlying reason — **a React effect cannot fix data that the current commit
 * already rendered with**:
 *
 *  1. A bare `Set` fetched in a parent effect: child effects run first, so the
 *     set was always empty at decision time and `auto_resume` never fired.
 *  2. A nullable `Set` cleared by the fetch effect on worktree change: on a
 *     *switch*, the previous worktree's set is still non-null in the very commit
 *     that mounts the new worktree's panes, so it read as an answer.
 *
 * Carrying the worktree id inside the answer makes staleness a *value* rather
 * than a timing property: it is checked during render, and there is nothing to
 * sequence. Returning `null` means "not known yet", which is what a pane waits
 * on — never "nothing to resume", which is what it would act on.
 */
export function paneAnswerFor(
  answer: PaneSessionAnswer | null,
  worktreeId: number,
): PaneSessionAnswer | null {
  return answer?.worktreeId === worktreeId ? answer : null;
}

/**
 * Panes the user has just asked for, awaiting their first mount.
 *
 * The materialization edge needs one bit that no amount of inspecting a pane
 * can recover: did this pane appear because somebody clicked "new Claude pane",
 * or because a stored layout was restored? The click is the consent for a fresh
 * launch, so [`configPaneTab`] records it here *before* the tab exists, and the
 * first mount spends it. Anything not in this set arrived from storage, and a
 * stored pane only ever starts on its own when `auto_resume` says so.
 *
 * It lives here rather than in `terminalHost` only to keep the imports acyclic —
 * that module already depends on this one.
 */
const PENDING_START = new Set<string>();

/** Note that the user just created this pane, so its first mount may launch. */
export function markPaneCreated(id: string): void {
  PENDING_START.add(id);
}

/** Whether this pane was just created by the user, consuming the fact. */
export function takePendingStart(id: string): boolean {
  return PENDING_START.delete(id);
}

/**
 * A tab for one of the project's own panes.
 *
 * The `title` is stored rather than derived, unlike a plain terminal's: a pane
 * whose spec has since been renamed or removed should keep reading as what the
 * user opened, not silently become "Terminal 2".
 */
export function configPaneTab(spec: { id: string; label: string }): PaneTab {
  const id = newTabId();
  // Before the tab exists, so the first mount can tell "the user just asked for
  // this" from "this came back from storage" — see `startPlanFor`.
  markPaneCreated(id);
  return { id, kind: "terminal", title: spec.label, spec: spec.id };
}

/** How a session should start. */
export type StartPlan = "shell" | "reattach" | PaneLaunchMode;

/** What a config-declared pane needs the host to remember about it. */
export interface PaneMount {
  spec: string;
  autoResume: boolean;
  closeOnExit: boolean;
}

/**
 * What a pane's *first* mount should do — the materialization edge.
 *
 * This runs once per session, because `ensure` is idempotent, and that is the
 * whole point: a pane only ever decides to launch something at the moment it
 * comes into being. A shell that dies later, while the user is watching, leaves
 * the pane in `ended` with buttons — no config flag can make that auto-restart,
 * because an exit you saw is one you get to answer.
 *
 *  - a plain terminal always starts its login shell, as it always has
 *  - a pane the user just asked for launches fresh; the click was the consent
 *  - a restored pane resumes only when the project asked for it
 *  - otherwise it reattaches, because its tool is very often still running
 */
export function startPlanFor(id: string, pane?: PaneMount): StartPlan {
  if (!pane) return "shell";
  if (takePendingStart(id)) return "fresh";
  if (pane.autoResume) return "resume";
  // **Reattach, not idle.** A restored pane's tool is very often still running:
  // a page reload, or a detach into a second window, gives a fresh renderer with
  // an empty `PENDING_START` while the holder never noticed. Sitting idle there
  // stranded a live agent behind two buttons that both `DELETE` its session —
  // so the pane killed the very thing it was restored to show. Attaching with no
  // mode asks the daemon "is it still there?"; if it is not, the guard in
  // `connect` drops to idle without spawning anything.
  return "reattach";
}

/**
 * Whether a config-declared pane should tidy itself away now that its command
 * has stopped.
 *
 * A pure function, and separated out because it is three conditions that each
 * guard a different real failure — inline in a render they read as one
 * condition and lose a clause quietly:
 *
 * - **`spec`** — a plain terminal has always stayed open when its shell exits,
 *   and this is not the change that alters that.
 * - **`closeOnExit`** — the project's own setting.
 * - **`exitCode === 0`** — a crash prints its reason on the screen it dies on,
 *   so closing takes the error with it. Note this reads the *code*, not the
 *   "ended" state: a takeover also ends a session, and a pane vanishing because
 *   another window claimed it is not tidying up.
 */
export function shouldCloseOnExit(pane: {
  spec: string | undefined;
  closeOnExit: boolean;
  exitCode: number | null;
}): boolean {
  return pane.spec !== undefined && pane.closeOnExit && pane.exitCode === 0;
}

/** A pane that has not chosen its content yet. */
export function newPaneTab(): PaneTab {
  return { id: newTabId(), kind: "new", title: "New pane" };
}

/**
 * The kinds that render a run view (`shared/RunViews.tsx`).
 *
 * Declared *from* `PaneKind` rather than as its own literal union: a second
 * hardcoded kind list is the trap this module warns about for `parseTab`, and
 * `Extract` means renaming a kind in `PANE_KINDS` collapses this to `never` and
 * breaks the build at the call sites instead of silently disagreeing.
 */
export type DiagKind = Extract<PaneKind, "logs" | "nodes">;

/**
 * A run-diagnostics tab.
 *
 * The stored `title` is only a fallback for a layout an older build wrote — the
 * label a tab strip shows comes from `paneTabLabel`, which derives it from the
 * kind. Both kinds render whichever run the selected worktree currently has, so
 * the tab holds no run identity of its own: switching worktrees re-points it,
 * which is the same rule the rest of IDE mode follows.
 */
export function diagTab(kind: DiagKind): PaneTab {
  return { id: newTabId(), kind, title: kind === "logs" ? "Logs" : "Nodes" };
}

/**
 * Bring a diagnostics pane to the front, adding one only if none is open.
 *
 * For a surface that means "take me to the diagnosis" rather than "give me
 * another pane" — the rail's attention affordance. Clicking it twice must not
 * grow a second Nodes tab, and a Nodes tab that is already open in the *other*
 * dock is the one to focus rather than duplicate, which `activateTab` handles by
 * moving `focused` to whichever dock holds it.
 *
 * Deliberately not used by the ⌘K "… in a pane" commands: those are explicitly
 * about opening a pane, and someone who asks for one twice wants two.
 */
export function revealDiagPane(layout: PaneLayout, kind: DiagKind): PaneLayout {
  const open = allTabs(layout).find((t) => t.kind === kind);
  return open ? activateTab(layout, open.id) : addTabToFocused(layout, diagTab(kind));
}

/**
 * Swap a tab's content, keeping its position and its active/focused state.
 *
 * The replacement carries a **new id**, which is the point: a terminal's id is
 * its daemon session and a browser view is keyed by id, so converting a `new`
 * pane must mint a fresh one rather than reuse the placeholder's.
 */
export function replaceTab(layout: PaneLayout, id: string, tab: PaneTab): PaneLayout {
  const index = dockOf(layout, id);
  if (index === null) return layout;
  // The replacement already existing elsewhere would put one id in two docks.
  if (hasTab(layout, tab.id) && tab.id !== id) return activateTab(layout, tab.id);
  const docks: [Dock, Dock] = [layout.docks[0], layout.docks[1]];
  const dock = docks[index];
  docks[index] = {
    tabs: dock.tabs.map((t) => (t.id === id ? tab : t)),
    activeId: dock.activeId === id ? tab.id : dock.activeId,
  };
  return { ...layout, docks, focused: index };
}

/**
 * Patch a tab in place.
 *
 * What a browser pane navigates to has to end up in the layout, or a reload
 * returns to wherever the pane was *opened* rather than where it was left. The
 * same object is returned when nothing changes, so a `did-navigate` that only
 * re-reports the current URL doesn't re-render the dock.
 */
export function updateTab(
  layout: PaneLayout,
  id: string,
  patch: Partial<Omit<PaneTab, "id" | "kind">>,
): PaneLayout {
  const index = dockOf(layout, id);
  if (index === null) return layout;
  const current = layout.docks[index].tabs.find((t) => t.id === id);
  if (!current) return layout;
  const next = { ...current, ...patch };
  if ((Object.keys(patch) as Array<keyof PaneTab>).every((k) => current[k] === next[k])) {
    return layout;
  }
  const docks: [Dock, Dock] = [layout.docks[0], layout.docks[1]];
  docks[index] = {
    ...docks[index],
    tabs: docks[index].tabs.map((t) => (t.id === id ? next : t)),
  };
  return { ...layout, docks };
}

/**
 * A tab's label.
 *
 * Derived from the kind rather than read from `tab.title` for everything except a
 * browser pane, whose title is the page's own. That keeps renaming a kind from
 * depending on what an older build wrote into storage, and it is the one place
 * tab naming lives.
 */
export function paneTabLabel(layout: PaneLayout, tab: PaneTab): string {
  switch (tab.kind) {
    case "terminal":
      // A config-declared pane is named by its spec, not numbered among the
      // shells: "Claude" and "Terminal 2" are different things in one strip.
      return tab.spec ? tab.title : terminalLabel(layout, tab.id);
    case "new":
      return "New pane";
    case "logs":
      return "Logs";
    case "nodes":
      return "Nodes";
    case "browser":
      return tab.title || urlLabel(tab.url);
  }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------
//
// **A worktree's panes live in the daemon's database**, one row per worktree
// (migration v14), read and written through `ide/layoutStore.ts`. What is left
// here is the storage for *detached* windows, which are a different thing: a
// detached window holds tabs transferred out of a worktree a main window owns,
// so it is a satellite of that window's claim rather than a view of the
// worktree, and its contents belong to the window and not to the checkout.
//
// Three mechanisms went with the move, and it is worth saying what they were so
// nobody reintroduces them: a shared `veld.panes.worktrees.v1` key written
// read-through by every window, a `restored` gate stopping a recycled slot from
// adopting a dead window's layout, and the window seed doubling as a layout
// source of last resort. All three existed because several clients wrote one key
// and none of them could see the others. One row with a version has no such
// problem — and, unlike the key, a browser tab can read it.

/**
 * A detached window's layout, per browser tab (`sessionStorage`).
 *
 * A layout names live daemon sessions, and two pages must not both claim the
 * same shell — an attach takes over, so shared ids would have them fighting over
 * one terminal. `sessionStorage` gives each page its own set and survives
 * exactly the event this exists for: a reload.
 */
export const LAYOUT_STORAGE_KEY = "veld.panes.v1";

/**
 * Where a detached Veld Desktop window's layout is *also* kept, so it survives
 * the app restarting.
 *
 * `sessionStorage` covers a reload, which is all a page needs within one run of
 * the app. An app restart is not: a Veld Desktop update replaces the window, and
 * a new window is a new `sessionStorage`, so the layout — and with it every
 * terminal's session id — was gone even though the holder processes had kept the
 * shells running. This key is the durable half, and it is deliberately **per
 * window slot**: two windows restoring one layout would both attach to one
 * shell, and an attach takes over, so they would ping-pong it forever.
 *
 * Only the Electron shell has slots (one per window, through the preload
 * bridge). A plain browser tab is never a detached window, so it never reaches
 * this.
 */
export function layoutSlotKey(slot: string): string {
  return `veld.panes.slot.${slot}.v1`;
}

export function serializeLayouts(layouts: Record<number, PaneLayout>): string {
  return JSON.stringify(layouts);
}

/** The two `getItem`/`setItem` calls this module needs, so tests can pass fakes
 *  and the real `Storage` objects satisfy it structurally. */
export interface LayoutStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/**
 * Restore a detached window's layouts, preferring what this very page last
 * wrote.
 *
 * Order matters. `sessionStorage` is *this page's* state and is therefore both
 * more recent and unambiguously ours; the slot store is what a previous run of
 * the app left behind. Reading the slot store first would let a stale layout
 * (written before a reload that changed things) win over the live one.
 *
 * **A main window gets nothing from here**, and that is the change: its panes
 * come from the daemon, which is the only answer that can be the same in a
 * desktop window and a browser tab.
 */
export function readLayouts(
  session: LayoutStorage | null,
  durable: LayoutStorage | null,
  slot: string | null,
  seed: string | null = null,
  /** Whether the shell *reopened* this window on a slot it owned before, rather
   *  than opening a new one that happened to be given the number. */
  restored = false,
  /** Only a detached window has a layout of its own — see the section comment. */
  satellite = false,
): Record<number, PaneLayout> {
  if (!satellite) return {};
  const own = parseLayouts(session?.getItem(LAYOUT_STORAGE_KEY) ?? null);
  if (Object.keys(own).length > 0) return own;
  // **Before the slot store, not after.** A seed exists only for a window the
  // shell created *this instant* by pulling tabs out of another one — a restored
  // window is opened with a slot and no seed — so its presence is proof that
  // nothing in the durable store can be this window's own state.
  //
  // Ordering it last was the first version and it was wrong, because slots are
  // *reused*: `nextSuffix` hands out the lowest free suffix counting live
  // windows only, and nothing ever clears `veld.panes.slot.<slot>.v1`. So
  // detach → close that window → detach again lands the new window on the same
  // slot, where it found the *previous* window's dead layout, returned it, and
  // discarded the seed. Both halves of that are silent: the tab actually being
  // moved exists in no layout at all (the origin already released and closed
  // it), so its shell dies at the detach grace — and the resurrected ids get
  // attached to, which *takes them over* from the window that just adopted them.
  const seeded = parseLayouts(seed);
  if (Object.keys(seeded).length > 0) return seeded;
  // **Only a window that was reopened may read the slot store.** Slots are
  // recycled and their keys are never cleared, so to a *new* window the stored
  // layout is a dead one that happens to share a number — and adopting it means
  // attaching to terminal ids another window is using, which takes those shells
  // over.
  if (!durable || !slot || !restored) return {};
  return parseLayouts(durable.getItem(layoutSlotKey(slot)) ?? null);
}

/**
 * Persist a detached window's layouts to both stores. The session copy is what a
 * reload reads; the durable copy is what the next launch reads.
 *
 * A main window writes nothing here — see `ide/layoutStore.ts`.
 */
export function writeLayouts(
  session: LayoutStorage | null,
  durable: LayoutStorage | null,
  slot: string | null,
  layouts: Record<number, PaneLayout>,
  satellite = false,
): void {
  if (!satellite) return;
  const serialized = serializeLayouts(layouts);
  session?.setItem(LAYOUT_STORAGE_KEY, serialized);
  if (!durable || !slot) return;
  durable.setItem(layoutSlotKey(slot), serialized);
}

/**
 * Every terminal id a detached window's own store knows about, for
 * `EXPECTED_RESUMES`.
 *
 * A main window's expected resumes come from the layout the daemon hands it —
 * see `noteExpectedResumes` in `terminalHost.ts`. Which is the fix for the case
 * that made this whole change necessary: a browser tab had no store, so every
 * terminal in a worktree the desktop app was running looked brand new to it.
 */
export function storedTerminalIds(slot: string | null, satellite: boolean): string[] {
  try {
    const { durable } = storages();
    if (!satellite || !durable || !slot) return [];
    return Object.values(parseLayouts(durable.getItem(layoutSlotKey(slot)) ?? null)).flatMap(
      terminalIds,
    );
  } catch {
    return [];
  }
}

/** The real storages, or `null` where they are unusable.
 *
 *  Storage *access* throws outright in some privacy configurations — not just
 *  `setItem` — and `loadLayouts` runs in a `useState` initialiser, where an
 *  exception white-screens the whole app before anything renders. */
function storages(): { session: LayoutStorage | null; durable: LayoutStorage | null } {
  let session: LayoutStorage | null = null;
  let durable: LayoutStorage | null = null;
  try {
    session = sessionStorage;
  } catch {
    // Leave it null.
  }
  try {
    durable = localStorage;
  } catch {
    // Leave it null.
  }
  return { session, durable };
}

/** Read the stored layouts, tolerating storage being unavailable. */
export function loadLayouts(
  slot: string | null,
  seed: string | null = null,
  restored = false,
  satellite = false,
): Record<number, PaneLayout> {
  try {
    const { session, durable } = storages();
    return readLayouts(session, durable, slot, seed, restored, satellite);
  } catch {
    return {};
  }
}

/** Save the layouts, tolerating storage being unavailable or full. */
export function saveLayouts(
  slot: string | null,
  layouts: Record<number, PaneLayout>,
  satellite = false,
): void {
  try {
    const { session, durable } = storages();
    writeLayouts(session, durable, slot, layouts, satellite);
  } catch {
    // The app keeps working; only the restore continuity is lost, and there is
    // nothing useful to tell the user.
  }
}

/**
 * Restore layouts, discarding anything that isn't the shape we wrote.
 *
 * This parses attacker-irrelevant but *stale* data: a layout written by an
 * older build, or hand-edited storage. A malformed entry must degrade to "no
 * saved layout for that worktree" rather than throw during the first render,
 * which would white-screen the whole app.
 */
export function parseLayouts(raw: string | null): Record<number, PaneLayout> {
  if (!raw) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return {};
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return {};

  const out: Record<number, PaneLayout> = {};
  for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
    const id = Number(key);
    if (!Number.isInteger(id)) continue;
    const layout = parseLayout(value);
    if (layout) out[id] = layout;
  }
  return out;
}

function parseTab(value: unknown): PaneTab | null {
  if (typeof value !== "object" || value === null) return null;
  const t = value as Record<string, unknown>;
  if (typeof t.id !== "string" || t.id === "") return null;
  // The id doubles as the daemon session id, which has a charset.
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(t.id)) return null;
  if (!isPaneKind(t.kind)) return null;
  const tab: PaneTab = {
    id: t.id,
    kind: t.kind,
    title: typeof t.title === "string" && t.title !== "" ? t.title : t.kind,
  };
  if (t.kind === "terminal") {
    // Same charset as a pane id in the config, so a hand-edited layout cannot
    // put anything else on the wire. An unrecognised spec is *not* dropped
    // here: the project's config is not loaded yet at parse time, and a pane
    // whose spec has since been renamed should say so rather than silently
    // becoming a login shell.
    if (typeof t.spec === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(t.spec)) {
      tab.spec = t.spec;
    }
  }
  if (t.kind === "browser") {
    // Re-validated on the way in, not only on the way out: storage is where a
    // stale build's — or a hand-edited — `javascript:` URL would sit waiting to
    // be handed to a view on restore.
    const url = typeof t.url === "string" ? normalizeBrowserUrl(t.url) : null;
    if (url) tab.url = url;
    tab.profile = isBrowserProfile(t.profile) ? t.profile : "default";
    // Same rule for the emulation: every number is clamped and the user-agent
    // string is re-checked, because this one ends up in a request header.
    const emulation = sanitizeEmulation(t.emulation);
    if (emulation) tab.emulation = emulation;
    const media = sanitizeMedia(t.media);
    if (media) tab.media = media;
    const zoom = sanitizeZoom(t.zoom);
    if (zoom !== null) tab.zoom = zoom;
  }
  return tab;
}

function parseDock(value: unknown): Dock {
  if (typeof value !== "object" || value === null) return { tabs: [], activeId: null };
  const d = value as Record<string, unknown>;
  const tabs = Array.isArray(d.tabs)
    ? d.tabs.map(parseTab).filter((t): t is PaneTab => t !== null)
    : [];
  // Deduplicate: two tabs sharing an id would fight over one shell.
  const seen = new Set<string>();
  const unique = tabs.filter((t) => !seen.has(t.id) && seen.add(t.id) !== undefined);
  const activeId =
    typeof d.activeId === "string" && unique.some((t) => t.id === d.activeId)
      ? d.activeId
      : (unique[0]?.id ?? null);
  return { tabs: unique, activeId };
}

/**
 * One worktree's layout, validated.
 *
 * Exported because the daemon store hands back a document it never looked
 * inside — see `migrate_v14_pane_layouts`. This is the gate: a layout written by
 * a newer build, or hand-edited in `sqlite3`, must degrade to "no saved layout"
 * rather than throw during a render.
 */
export function parseLayout(value: unknown): PaneLayout | null {
  if (typeof value !== "object" || value === null) return null;
  const l = value as Record<string, unknown>;
  if (!Array.isArray(l.docks) || l.docks.length !== 2) return null;

  const docks: [Dock, Dock] = [parseDock(l.docks[0]), parseDock(l.docks[1])];
  // An id must not appear in both docks.
  const left = new Set(docks[0].tabs.map((t) => t.id));
  if (docks[1].tabs.some((t) => left.has(t.id))) {
    docks[1] = parseDock({
      tabs: docks[1].tabs.filter((t) => !left.has(t.id)),
      activeId: docks[1].activeId,
    });
  }
  if (docks[0].tabs.length === 0 && docks[1].tabs.length === 0) return null;

  let focused: DockIndex = l.focused === 1 ? 1 : 0;
  // Focus must land on a dock that is actually rendered.
  if (docks[focused].tabs.length === 0) focused = focused === 0 ? 1 : 0;

  // Also normalised on restore: a layout written before this rule existed, or by
  // an older build, can be left-empty.
  return normalizeDocks({ docks, ratio: clampRatio(Number(l.ratio)), focused });
}

/** Sequentially numbered within the layout, so titles read "terminal 2" and
 *  not "term-7". Recomputed on render rather than stored: closing terminal 1
 *  should renumber the rest, the way tab titles behave elsewhere. */
export function terminalLabel(layout: PaneLayout, id: string): string {
  const ids = terminalIds(layout);
  const n = ids.indexOf(id);
  if (n < 0) return "Terminal";
  return ids.length > 1 ? `Terminal ${n + 1}` : "Terminal";
}
