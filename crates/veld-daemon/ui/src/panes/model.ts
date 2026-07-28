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

/** What a tab shows. New content types extend this union. */
export type PaneKind = "services" | "terminal";

export interface PaneTab {
  /** Stable across re-renders and worktree switches — it keys the live
   *  terminal in `terminalHost`, so reusing an id reuses a shell. */
  id: string;
  kind: PaneKind;
  title: string;
}

export interface Dock {
  tabs: PaneTab[];
  /** `null` only when the dock is empty. */
  activeId: string | null;
}

/** Which of the two docks: 0 is the left/primary. */
export type DockIndex = 0 | 1;

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

export const SERVICES_TAB_ID = "services";

export function clampRatio(r: number): number {
  if (!Number.isFinite(r)) return DEFAULT_RATIO;
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, r));
}

/**
 * The layout a worktree starts with: a terminal on the left, the service
 * launcher on the right. Mirrors the old fixed columns so the change of
 * mechanism isn't also a change of what people see on open.
 */
export function defaultLayout(ratio = DEFAULT_RATIO): PaneLayout {
  const terminal: PaneTab = { id: newTerminalId(), kind: "terminal", title: "terminal" };
  const services: PaneTab = { id: SERVICES_TAB_ID, kind: "services", title: "services" };
  return {
    docks: [
      { tabs: [terminal], activeId: terminal.id },
      { tabs: [services], activeId: services.id },
    ],
    ratio: clampRatio(ratio),
    focused: 0,
  };
}

/**
 * A terminal tab's id is also its **daemon session id** — the name the page
 * uses to ask for the same shell back after a reload (see
 * `crates/veld-daemon/src/pty.rs`).
 *
 * So it must be unique for longer than the page: reusing one adopts whatever
 * shell still answers to it. It is an identifier, not a credential — an attach
 * is authorised by the CSRF-gated ticket and the `Origin` allowlist, not by
 * knowing this string — so the non-crypto fallback below is not a weakness,
 * just a less collision-proof name. The daemon accepts `[A-Za-z0-9_-]{1,64}`.
 */
export function newTerminalId(): string {
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
  return { ...layout, docks, focused };
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
  return { ...layout, docks, focused: dock };
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

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/**
 * Layouts are stored per **browser tab** (`sessionStorage`), not per browser.
 *
 * A layout names live daemon sessions, and two tabs of `/ide` must not both
 * claim the same shell — an attach takes over, so shared ids would have the
 * tabs fighting over one terminal. `sessionStorage` gives each tab its own set
 * and survives exactly the event this exists for: a reload.
 */
export const LAYOUT_STORAGE_KEY = "veld.panes.v1";

export function serializeLayouts(layouts: Record<number, PaneLayout>): string {
  return JSON.stringify(layouts);
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
  if (t.kind !== "services" && t.kind !== "terminal") return null;
  return {
    id: t.id,
    kind: t.kind,
    title: typeof t.title === "string" && t.title !== "" ? t.title : t.kind,
  };
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

function parseLayout(value: unknown): PaneLayout | null {
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

  return { docks, ratio: clampRatio(Number(l.ratio)), focused };
}

/** Sequentially numbered within the layout, so titles read "terminal 2" and
 *  not "term-7". Recomputed on render rather than stored: closing terminal 1
 *  should renumber the rest, the way tab titles behave elsewhere. */
export function terminalLabel(layout: PaneLayout, id: string): string {
  const ids = terminalIds(layout);
  const n = ids.indexOf(id);
  if (n < 0) return "terminal";
  return ids.length > 1 ? `terminal ${n + 1}` : "terminal";
}
