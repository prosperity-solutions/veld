import { describe, expect, it } from "vitest";
import {
  DEFAULT_RATIO,
  type DockIndex,
  MAX_RATIO,
  MIN_RATIO,
  type PaneLayout,
  type PaneTab,
  SERVICES_TAB_ID,
  activateTab,
  activeTab,
  addTab,
  addTabToFocused,
  allTabs,
  clampRatio,
  closeTab,
  defaultLayout,
  dockVisible,
  focusDock,
  hasTab,
  moveTab,
  moveTabToOtherDock,
  newTerminalId,
  parseLayouts,
  serializeLayouts,
  setRatio,
  terminalIds,
  terminalLabel,
} from "./model";

const term = (): PaneTab => ({
  id: newTerminalId(),
  kind: "terminal",
  title: "terminal",
});

const ids = (layout: PaneLayout, dock: DockIndex) =>
  layout.docks[dock].tabs.map((t) => t.id);

describe("defaultLayout", () => {
  it("opens a terminal beside the services tab", () => {
    const l = defaultLayout();
    expect(l.docks[0].tabs).toHaveLength(1);
    expect(l.docks[0].tabs[0].kind).toBe("terminal");
    expect(l.docks[1].tabs[0].id).toBe(SERVICES_TAB_ID);
    // Each dock shows the only tab it has; a null activeId would render empty.
    expect(l.docks[0].activeId).toBe(l.docks[0].tabs[0].id);
    expect(l.docks[1].activeId).toBe(SERVICES_TAB_ID);
    expect(l.focused).toBe(0);
  });

  it("seeds the split from the caller and clamps a nonsense value", () => {
    expect(defaultLayout(0.3).ratio).toBe(0.3);
    expect(defaultLayout(0).ratio).toBe(MIN_RATIO);
    // The seed comes from localStorage, so App.tsx parses it with parseFloat:
    // an empty or corrupt value must land on an even split, not on the
    // minimum (which `Number("")` → 0 → clamp would have given).
    expect(defaultLayout(Number.parseFloat("")).ratio).toBe(DEFAULT_RATIO);
    expect(defaultLayout(Number.parseFloat("nope")).ratio).toBe(DEFAULT_RATIO);
    expect(defaultLayout(Number.parseFloat("0.35")).ratio).toBe(0.35);
  });
});

describe("terminal ids", () => {
  it("never repeats, even after the tab that used one is closed", () => {
    // A reused id would adopt the closed tab's live shell in terminalHost.
    let l = defaultLayout();
    const first = l.docks[0].tabs[0].id;
    l = closeTab(l, first);
    const second = term().id;
    expect(second).not.toBe(first);
  });

  it("stays inside the charset the daemon accepts for a session id", () => {
    // The id is sent as `session_id`; pty.rs rejects anything outside
    // [A-Za-z0-9_-]{1,64}, which would make the terminal un-openable.
    for (let i = 0; i < 50; i += 1) {
      expect(newTerminalId()).toMatch(/^[A-Za-z0-9_-]{1,64}$/);
    }
  });

  it("does not collide across many ids", () => {
    const ids = new Set(Array.from({ length: 500 }, () => newTerminalId()));
    expect(ids.size).toBe(500);
  });
});

describe("clampRatio", () => {
  it("keeps a dock from collapsing to a sliver", () => {
    expect(clampRatio(-1)).toBe(MIN_RATIO);
    expect(clampRatio(2)).toBe(MAX_RATIO);
    expect(clampRatio(0.42)).toBe(0.42);
  });

  it("falls back rather than propagating NaN into a width", () => {
    expect(clampRatio(NaN)).toBe(DEFAULT_RATIO);
    expect(clampRatio(Infinity)).toBe(DEFAULT_RATIO);
  });
});

describe("addTab", () => {
  it("appends, activates, and takes focus", () => {
    let l = defaultLayout();
    const t = term();
    l = addTab(l, 1, t);
    expect(ids(l, 1)).toEqual([SERVICES_TAB_ID, t.id]);
    expect(l.docks[1].activeId).toBe(t.id);
    expect(l.focused).toBe(1);
  });

  it("activates an existing tab instead of duplicating its id", () => {
    // Two tabs with one id would fight over a single live terminal.
    let l = defaultLayout();
    const existing = l.docks[0].tabs[0];
    l = activateTab(l, SERVICES_TAB_ID);
    l = addTab(l, 1, existing);
    expect(allTabs(l).filter((t) => t.id === existing.id)).toHaveLength(1);
    expect(ids(l, 1)).toEqual([SERVICES_TAB_ID]);
    expect(l.docks[0].activeId).toBe(existing.id);
    expect(l.focused).toBe(0);
  });

  it("addTabToFocused follows the focused dock", () => {
    let l = focusDock(defaultLayout(), 1);
    const t = term();
    l = addTabToFocused(l, t);
    expect(ids(l, 1)).toContain(t.id);
    expect(ids(l, 0)).not.toContain(t.id);
  });
});

describe("closeTab", () => {
  it("activates the tab to the right, then the one to the left", () => {
    let l = defaultLayout();
    const a = l.docks[0].tabs[0];
    const b = term();
    const c = term();
    l = addTab(l, 0, b);
    l = addTab(l, 0, c);
    l = activateTab(l, b.id);

    l = closeTab(l, b.id);
    expect(l.docks[0].activeId).toBe(c.id);

    l = closeTab(l, c.id);
    expect(l.docks[0].activeId).toBe(a.id);
  });

  it("leaves the active tab alone when a different one closes", () => {
    let l = defaultLayout();
    const a = l.docks[0].tabs[0];
    const b = term();
    l = addTab(l, 0, b);
    l = activateTab(l, a.id);
    l = closeTab(l, b.id);
    expect(l.docks[0].activeId).toBe(a.id);
  });

  it("hides an emptied dock and moves focus to the one with tabs", () => {
    let l = defaultLayout();
    const only = l.docks[0].tabs[0];
    l = closeTab(l, only.id);
    expect(dockVisible(l, 0)).toBe(false);
    expect(l.docks[0].activeId).toBeNull();
    // Otherwise the next "new terminal" would open into a hidden column.
    expect(l.focused).toBe(1);
  });

  it("keeps focus put when the emptied dock was not the focused one", () => {
    let l = defaultLayout();
    l = closeTab(l, SERVICES_TAB_ID);
    expect(dockVisible(l, 1)).toBe(false);
    expect(l.focused).toBe(0);
  });

  it("is a no-op for an unknown id", () => {
    const l = defaultLayout();
    expect(closeTab(l, "nope")).toBe(l);
  });

  it("survives closing every tab", () => {
    let l = defaultLayout();
    for (const t of allTabs(l)) l = closeTab(l, t.id);
    expect(allTabs(l)).toHaveLength(0);
    expect(dockVisible(l, 0)).toBe(false);
    expect(dockVisible(l, 1)).toBe(false);
    expect(activeTab(l, 0)).toBeNull();
    // A new tab must still land somewhere reachable.
    const t = term();
    l = addTabToFocused(l, t);
    expect(dockVisible(l, l.focused)).toBe(true);
    expect(activeTab(l, l.focused)?.id).toBe(t.id);
  });
});

describe("moveTabToOtherDock", () => {
  it("moves and keeps the tab active in its new dock", () => {
    let l = defaultLayout();
    const a = l.docks[0].tabs[0];
    const b = term();
    l = addTab(l, 0, b);

    l = moveTabToOtherDock(l, b.id);
    expect(ids(l, 0)).toEqual([a.id]);
    expect(ids(l, 1)).toEqual([SERVICES_TAB_ID, b.id]);
    expect(l.docks[1].activeId).toBe(b.id);
  });

  it("refuses to move a dock's last tab, which would just swap sides", () => {
    const l = defaultLayout();
    expect(moveTabToOtherDock(l, SERVICES_TAB_ID)).toBe(l);
  });

  it("is a no-op for an unknown id", () => {
    const l = defaultLayout();
    expect(moveTabToOtherDock(l, "nope")).toBe(l);
  });
});

describe("moveTab", () => {
  it("reorders within a dock, in post-removal terms", () => {
    let l = defaultLayout();
    const a = l.docks[0].tabs[0];
    const b = term();
    const c = term();
    l = addTab(l, 0, b);
    l = addTab(l, 0, c);
    expect(ids(l, 0)).toEqual([a.id, b.id, c.id]);

    // Send the first tab to the end.
    l = moveTab(l, a.id, 0, 2);
    expect(ids(l, 0)).toEqual([b.id, c.id, a.id]);

    // And back to the front.
    l = moveTab(l, a.id, 0, 0);
    expect(ids(l, 0)).toEqual([a.id, b.id, c.id]);
  });

  it("inserts at a position when moving across docks", () => {
    let l = defaultLayout();
    const a = l.docks[0].tabs[0];
    const b = term();
    l = addTab(l, 0, b);

    l = moveTab(l, b.id, 1, 0);
    expect(ids(l, 0)).toEqual([a.id]);
    expect(ids(l, 1)).toEqual([b.id, SERVICES_TAB_ID]);
    expect(l.docks[1].activeId).toBe(b.id);
    expect(l.focused).toBe(1);
  });

  it("returns the same object for a drop that changes nothing", () => {
    // A stray drag must not churn React or reset the dock's active tab.
    let l = defaultLayout();
    const b = term();
    l = addTab(l, 0, b);
    l = activateTab(l, l.docks[0].tabs[0].id);
    const at = l.docks[0].tabs.findIndex((t) => t.id === b.id);
    expect(moveTab(l, b.id, 0, at)).toBe(l);
  });

  it("clamps an out-of-range index instead of dropping the tab", () => {
    let l = defaultLayout();
    const b = term();
    l = addTab(l, 0, b);
    const moved = moveTab(l, b.id, 1, 99);
    expect(ids(moved, 1)).toEqual([SERVICES_TAB_ID, b.id]);
    const front = moveTab(l, b.id, 1, -5);
    expect(ids(front, 1)).toEqual([b.id, SERVICES_TAB_ID]);
  });

  it("hands the vacated dock a new active tab, or none", () => {
    let l = defaultLayout();
    const a = l.docks[0].tabs[0];
    const b = term();
    l = addTab(l, 0, b);
    // `b` is active in dock 0; moving it away must promote `a`.
    l = moveTab(l, b.id, 1);
    expect(l.docks[0].activeId).toBe(a.id);

    // Moving the last one out empties the dock.
    l = moveTab(l, a.id, 1);
    expect(l.docks[0].tabs).toHaveLength(0);
    expect(l.docks[0].activeId).toBeNull();
    expect(dockVisible(l, 0)).toBe(false);
  });

  it("leaves an unrelated active tab alone", () => {
    let l = defaultLayout();
    const a = l.docks[0].tabs[0];
    const b = term();
    l = addTab(l, 0, b);
    l = activateTab(l, a.id);
    l = moveTab(l, b.id, 1);
    expect(l.docks[0].activeId).toBe(a.id);
  });

  it("is a no-op for an unknown id", () => {
    const l = defaultLayout();
    expect(moveTab(l, "nope", 1, 0)).toBe(l);
  });
});

describe("activateTab / focusDock", () => {
  it("activating a tab focuses the dock holding it", () => {
    const l = activateTab(defaultLayout(), SERVICES_TAB_ID);
    expect(l.focused).toBe(1);
    expect(l.docks[1].activeId).toBe(SERVICES_TAB_ID);
  });

  it("activating an unknown id changes nothing observable", () => {
    const l = defaultLayout();
    const after = activateTab(l, "nope");
    expect(after.docks[0].activeId).toBe(l.docks[0].activeId);
    expect(after.focused).toBe(l.focused);
  });

  it("focusDock returns the same object when nothing changes", () => {
    // Focus events fire on every click inside a dock; a new object each time
    // would re-render the whole region for nothing.
    const l = defaultLayout();
    expect(focusDock(l, 0)).toBe(l);
  });

  it("refuses to focus an empty dock", () => {
    let l = defaultLayout();
    l = closeTab(l, SERVICES_TAB_ID);
    expect(focusDock(l, 1)).toBe(l);
  });
});

describe("setRatio", () => {
  it("clamps through the same rule as the seed", () => {
    expect(setRatio(defaultLayout(), 0.95).ratio).toBe(MAX_RATIO);
    expect(setRatio(defaultLayout(), 0.4).ratio).toBe(0.4);
  });
});

describe("terminal bookkeeping", () => {
  it("terminalIds lists only terminals, across both docks", () => {
    let l = defaultLayout();
    const b = term();
    l = addTab(l, 1, b);
    expect(terminalIds(l)).toEqual([l.docks[0].tabs[0].id, b.id]);
    expect(terminalIds(l)).not.toContain(SERVICES_TAB_ID);
  });

  it("hasTab sees both docks", () => {
    const l = defaultLayout();
    expect(hasTab(l, SERVICES_TAB_ID)).toBe(true);
    expect(hasTab(l, l.docks[0].tabs[0].id)).toBe(true);
    expect(hasTab(l, "nope")).toBe(false);
  });

  it("numbers terminals only once there is more than one", () => {
    let l = defaultLayout();
    const a = l.docks[0].tabs[0];
    expect(terminalLabel(l, a.id)).toBe("terminal");

    const b = term();
    l = addTab(l, 0, b);
    expect(terminalLabel(l, a.id)).toBe("terminal 1");
    expect(terminalLabel(l, b.id)).toBe("terminal 2");

    // Renumbered on close rather than left with a gap, the way tab titles
    // behave elsewhere.
    l = closeTab(l, a.id);
    expect(terminalLabel(l, b.id)).toBe("terminal");
  });

  it("labels an unknown id without throwing", () => {
    expect(terminalLabel(defaultLayout(), "nope")).toBe("terminal");
  });
});

describe("persistence", () => {
  it("round-trips a layout", () => {
    let l = defaultLayout(0.4);
    l = addTab(l, 0, term());
    const back = parseLayouts(serializeLayouts({ 7: l }));
    expect(back[7]).toEqual(l);
  });

  it("returns nothing for absent or unparseable storage", () => {
    // Must degrade to "no saved layout", never throw during first render.
    expect(parseLayouts(null)).toEqual({});
    expect(parseLayouts("")).toEqual({});
    expect(parseLayouts("not json")).toEqual({});
    expect(parseLayouts("[]")).toEqual({});
    expect(parseLayouts("null")).toEqual({});
    expect(parseLayouts('"a string"')).toEqual({});
    expect(parseLayouts("42")).toEqual({});
  });

  it("drops entries whose shape is not a layout", () => {
    expect(parseLayouts('{"1":null}')).toEqual({});
    expect(parseLayouts('{"1":{"docks":[]}}')).toEqual({});
    expect(parseLayouts('{"1":{"docks":[{},{}]}}')).toEqual({});
    // Non-numeric worktree keys can't index anything.
    const layout = defaultLayout();
    const ok = parseLayouts(JSON.stringify({ abc: layout, 3: layout }));
    expect(Object.keys(ok)).toEqual(["3"]);
  });

  it("drops tabs with an unusable kind or id", () => {
    const raw = JSON.stringify({
      1: {
        docks: [
          {
            tabs: [
              { id: "good-1", kind: "terminal", title: "t" },
              { id: "bad kind", kind: "browser", title: "b" },
              // Outside the daemon's session-id charset.
              { id: "../etc", kind: "terminal", title: "x" },
              { id: "", kind: "terminal", title: "y" },
              { kind: "terminal", title: "z" },
            ],
            activeId: "good-1",
          },
          { tabs: [], activeId: null },
        ],
        ratio: 0.5,
        focused: 0,
      },
    });
    const l = parseLayouts(raw)[1];
    expect(l.docks[0].tabs.map((t) => t.id)).toEqual(["good-1"]);
  });

  it("never restores one id into two tabs", () => {
    // Two tabs sharing an id would both attach to one daemon session, and the
    // second attach would take the shell away from the first.
    const raw = JSON.stringify({
      1: {
        docks: [
          {
            tabs: [
              { id: "dup", kind: "terminal", title: "t" },
              { id: "dup", kind: "terminal", title: "t" },
            ],
            activeId: "dup",
          },
          { tabs: [{ id: "dup", kind: "terminal", title: "t" }], activeId: "dup" },
        ],
        ratio: 0.5,
        focused: 0,
      },
    });
    const l = parseLayouts(raw)[1];
    expect(allTabs(l).filter((t) => t.id === "dup")).toHaveLength(1);
  });

  it("repairs an activeId and a focus that point nowhere", () => {
    const raw = JSON.stringify({
      1: {
        docks: [
          { tabs: [{ id: "a", kind: "terminal", title: "t" }], activeId: "gone" },
          { tabs: [], activeId: "also-gone" },
        ],
        ratio: 0.5,
        // Focused on the empty dock, which is not rendered at all.
        focused: 1,
      },
    });
    const l = parseLayouts(raw)[1];
    expect(l.docks[0].activeId).toBe("a");
    expect(l.docks[1].activeId).toBeNull();
    expect(l.focused).toBe(0);
  });

  it("clamps a corrupt ratio rather than laying out with NaN", () => {
    const of = (ratio: unknown) =>
      parseLayouts(
        JSON.stringify({
          1: {
            docks: [
              { tabs: [{ id: "a", kind: "terminal", title: "t" }], activeId: "a" },
              { tabs: [], activeId: null },
            ],
            ratio,
            focused: 0,
          },
        }),
      )[1].ratio;
    expect(of(9)).toBe(MAX_RATIO);
    expect(of(-9)).toBe(MIN_RATIO);
    expect(of("nope")).toBe(DEFAULT_RATIO);
    expect(of(null)).toBe(MIN_RATIO);
    expect(of(undefined)).toBe(DEFAULT_RATIO);
  });

  it("drops a layout with no tabs at all", () => {
    // Nothing to render, and it would suppress the default layout.
    const raw = JSON.stringify({
      1: { docks: [{ tabs: [], activeId: null }, { tabs: [], activeId: null }], ratio: 0.5, focused: 0 },
    });
    expect(parseLayouts(raw)).toEqual({});
  });
});
