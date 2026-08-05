import { describe, expect, it } from "vitest";
import {
  DEFAULT_RATIO,
  type DockIndex,
  MAX_RATIO,
  MIN_RATIO,
  BROWSER_PROFILES,
  BROWSER_PROFILE_COLORS,
  type BrowserProfile,
  MAX_EXTRA_SESSIONS,
  PANE_KINDS,
  type PaneLayout,
  type PaneTab,
  activateTab,
  activeTab,
  addTab,
  addTabToFocused,
  adoptTabs,
  allTabs,
  browserIds,
  browserProfileLabel,
  browserTab,
  clampRatio,
  closeTab,
  defaultLayout,
  diagTab,
  dockVisible,
  dropWorktreeLayouts,
  findTab,
  focusDock,
  hasTab,
  insertTab,
  isVeldOwnUi,
  lastBlankBrowserId,
  moveTab,
  moveTabToOtherDock,
  newPaneTab,
  newTabId,
  nextFreeProfile,
  normalizeBrowserUrl,
  normalizeSessionSet,
  LAYOUT_WORKTREE_KEY,
  layoutSlotKey,
  parseLayouts,
  paneTabLabel,
  parseSessionSets,
  parseTransferTabs,
  replaceTab,
  revealDiagPane,
  readLayouts,
  serializeLayouts,
  splitWithTab,
  serializeSessionSets,
  sessionSetFor,
  sessionsInUse,
  setRatio,
  terminalIds,
  terminalLabel,
  worktreeLayoutFrom,
  writeLayouts,
  updateTab,
  urlLabel,
} from "./model";

const term = (): PaneTab => ({
  id: newTabId(),
  kind: "terminal",
  title: "terminal",
});

const ids = (layout: PaneLayout, dock: DockIndex) =>
  layout.docks[dock].tabs.map((t) => t.id);

/** The default layout's right-hand pane. Captured per test rather than named by a
 *  constant: there is no singleton tab any more, so its id is generated. */
const rightId = (layout: PaneLayout) => layout.docks[1].tabs[0].id;

describe("defaultLayout", () => {
  it("opens a chooser beside an empty browser pane, and starts no shell", () => {
    // The right-hand pane shows the run's URLs until it is pointed somewhere, so
    // this is the same information the old services column carried — in the thing
    // that can act on it.
    //
    // The left one is a `new` pane, deliberately: a terminal tab's id is a daemon
    // session id, so seeding one made merely *selecting* a worktree start a real
    // shell (and now a holder process to own it), against a cap of 48.
    const l = defaultLayout();
    expect(l.docks[0].tabs).toHaveLength(1);
    expect(l.docks[0].tabs[0].kind).toBe("new");
    expect(terminalIds(l)).toEqual([]);
    expect(l.docks[1].tabs).toHaveLength(1);
    expect(l.docks[1].tabs[0].kind).toBe("browser");
    expect(l.docks[1].tabs[0].url).toBeUndefined();
    // Each dock shows the only tab it has; a null activeId would render empty.
    expect(l.docks[0].activeId).toBe(l.docks[0].tabs[0].id);
    expect(l.docks[1].activeId).toBe(l.docks[1].tabs[0].id);
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
      expect(newTabId()).toMatch(/^[A-Za-z0-9_-]{1,64}$/);
    }
  });

  it("does not collide across many ids", () => {
    const ids = new Set(Array.from({ length: 500 }, () => newTabId()));
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
    const svc = rightId(l);
    const t = term();
    l = addTab(l, 1, t);
    expect(ids(l, 1)).toEqual([svc, t.id]);
    expect(l.docks[1].activeId).toBe(t.id);
    expect(l.focused).toBe(1);
  });

  it("activates an existing tab instead of duplicating its id", () => {
    // Two tabs with one id would fight over a single live terminal.
    let l = defaultLayout();
    const svc = rightId(l);
    const existing = l.docks[0].tabs[0];
    l = activateTab(l, svc);
    l = addTab(l, 1, existing);
    expect(allTabs(l).filter((t) => t.id === existing.id)).toHaveLength(1);
    expect(ids(l, 1)).toEqual([svc]);
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

  it("slides the survivors left when the left dock empties", () => {
    let l = defaultLayout();
    const svc = rightId(l);
    const only = l.docks[0].tabs[0];
    l = closeTab(l, only.id);
    // With one pane on screen there is no left or right, so the survivor must be
    // the left dock — otherwise the tab menu offers "Move to the left pane" with
    // nothing to the left of anything.
    expect(dockVisible(l, 0)).toBe(true);
    expect(l.docks[0].tabs.map((t) => t.id)).toEqual([svc]);
    expect(dockVisible(l, 1)).toBe(false);
    expect(l.docks[1].activeId).toBeNull();
    // Otherwise the next "new terminal" would open into a hidden column.
    expect(l.focused).toBe(0);
  });

  it("keeps focus put when the emptied dock was not the focused one", () => {
    let l = defaultLayout();
    const svc = rightId(l);
    l = closeTab(l, svc);
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
    const svc = rightId(l);
    const a = l.docks[0].tabs[0];
    const b = term();
    l = addTab(l, 0, b);

    l = moveTabToOtherDock(l, b.id);
    expect(ids(l, 0)).toEqual([a.id]);
    expect(ids(l, 1)).toEqual([svc, b.id]);
    expect(l.docks[1].activeId).toBe(b.id);
  });

  it("refuses to move a dock's last tab, which would just swap sides", () => {
    const l = defaultLayout();
    const svc = rightId(l);
    expect(moveTabToOtherDock(l, svc)).toBe(l);
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
    const svc = rightId(l);
    const a = l.docks[0].tabs[0];
    const b = term();
    l = addTab(l, 0, b);

    l = moveTab(l, b.id, 1, 0);
    expect(ids(l, 0)).toEqual([a.id]);
    expect(ids(l, 1)).toEqual([b.id, svc]);
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
    const svc = rightId(l);
    const b = term();
    l = addTab(l, 0, b);
    const moved = moveTab(l, b.id, 1, 99);
    expect(ids(moved, 1)).toEqual([svc, b.id]);
    const front = moveTab(l, b.id, 1, -5);
    expect(ids(front, 1)).toEqual([b.id, svc]);
  });

  it("hands the vacated dock a new active tab, or none", () => {
    let l = defaultLayout();
    const svc = rightId(l);
    const a = l.docks[0].tabs[0];
    const b = term();
    l = addTab(l, 0, b);
    // `b` is active in dock 0; moving it away must promote `a`.
    l = moveTab(l, b.id, 1);
    expect(l.docks[0].activeId).toBe(a.id);

    // Moving the last one out empties the left dock — which then slides the
    // right one over, so what was dock 1 is dock 0 and nothing is hidden.
    l = moveTab(l, a.id, 1);
    expect(l.docks[0].tabs.map((t) => t.id)).toEqual([svc, b.id, a.id]);
    expect(l.docks[1].tabs).toHaveLength(0);
    expect(l.docks[1].activeId).toBeNull();
    expect(l.focused).toBe(0);
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
    const base = defaultLayout();
    const right = rightId(base);
    const l = activateTab(base, right);
    expect(l.focused).toBe(1);
    expect(l.docks[1].activeId).toBe(right);
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
    const svc = rightId(l);
    l = closeTab(l, svc);
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
    const svc = rightId(l);
    const a = term();
    const b = term();
    l = addTab(l, 0, a);
    l = addTab(l, 1, b);
    expect(terminalIds(l)).toEqual([a.id, b.id]);
    expect(terminalIds(l)).not.toContain(svc);
    // The seeded chooser is not a terminal and must never be counted as one.
    expect(terminalIds(l)).not.toContain(l.docks[0].tabs[0].id);
  });

  it("hasTab sees both docks", () => {
    const l = defaultLayout();
    const svc = rightId(l);
    expect(hasTab(l, svc)).toBe(true);
    expect(hasTab(l, l.docks[0].tabs[0].id)).toBe(true);
    expect(hasTab(l, "nope")).toBe(false);
  });

  it("numbers terminals only once there is more than one", () => {
    let l = defaultLayout();
    const a = term();
    l = addTab(l, 0, a);
    expect(terminalLabel(l, a.id)).toBe("Terminal");

    const b = term();
    l = addTab(l, 0, b);
    expect(terminalLabel(l, a.id)).toBe("Terminal 1");
    expect(terminalLabel(l, b.id)).toBe("Terminal 2");

    // Renumbered on close rather than left with a gap, the way tab titles
    // behave elsewhere.
    l = closeTab(l, a.id);
    expect(terminalLabel(l, b.id)).toBe("Terminal");
  });

  it("labels an unknown id without throwing", () => {
    expect(terminalLabel(defaultLayout(), "nope")).toBe("Terminal");
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

  it("accepts every declared pane kind", () => {
    // PANE_KINDS and the restore validator must agree, or a new content type
    // works until the first reload and then vanishes silently.
    for (const kind of PANE_KINDS) {
      const raw = JSON.stringify({
        1: {
          docks: [
            { tabs: [{ id: "a", kind, title: "t" }], activeId: "a" },
            { tabs: [], activeId: null },
          ],
          ratio: 0.5,
          focused: 0,
        },
      });
      expect(parseLayouts(raw)[1]?.docks[0].tabs[0].kind).toBe(kind);
    }
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

describe("browser tabs", () => {
  it("only accepts http(s), and completes a bare host", () => {
    expect(normalizeBrowserUrl("https://web.dev.veld.localhost/")).toBe(
      "https://web.dev.veld.localhost/",
    );
    expect(normalizeBrowserUrl("http://127.0.0.1:3000/x?y=1")).toBe(
      "http://127.0.0.1:3000/x?y=1",
    );
    // Typing a bare address into an address bar is the normal way to use one.
    expect(normalizeBrowserUrl("localhost:3000")).toBe("https://localhost:3000/");
    expect(normalizeBrowserUrl("  example.test/path  ")).toBe("https://example.test/path");

    // Everything else is refused rather than handed to a view: `javascript:`
    // would be script in the pane's origin, `file:` a local-file reader.
    expect(normalizeBrowserUrl("javascript:alert(1)")).toBeNull();
    expect(normalizeBrowserUrl("file:///etc/passwd")).toBeNull();
    expect(normalizeBrowserUrl("data:text/html,<b>x")).toBeNull();
    expect(normalizeBrowserUrl("chrome://settings")).toBeNull();
    expect(normalizeBrowserUrl("about:blank")).toBeNull();
    expect(normalizeBrowserUrl("")).toBeNull();
    expect(normalizeBrowserUrl("   ")).toBeNull();
    expect(normalizeBrowserUrl("https://")).toBeNull();
  });

  describe("isVeldOwnUi", () => {
    const self = "http://127.0.0.1:19899";

    it("catches this app's own two paths on the origin it was loaded from", () => {
      expect(isVeldOwnUi("http://127.0.0.1:19899/ide", self)).toBe(true);
      expect(isVeldOwnUi("http://127.0.0.1:19899/ide/", self)).toBe(true);
      expect(isVeldOwnUi("http://127.0.0.1:19899/ide?view=runs", self)).toBe(true);
      expect(isVeldOwnUi("http://127.0.0.1:19899/ide/anything", self)).toBe(true);
      // `/` is the dashboard — the same non-use, and the one place a *click* could
      // otherwise reach a nested `/ide` without passing through here.
      expect(isVeldOwnUi("http://127.0.0.1:19899/", self)).toBe(true);
      expect(isVeldOwnUi("http://127.0.0.1:19899", self)).toBe(true);
    });

    it("catches the Caddy-fronted name too, on any port", () => {
      // The daemon is reachable two ways and the app only knows the one it was
      // loaded from, so the other is matched by name.
      expect(isVeldOwnUi("https://veld.localhost/ide", self)).toBe(true);
      expect(isVeldOwnUi("https://veld.localhost:18443/ide", self)).toBe(true);
      expect(isVeldOwnUi("https://veld.localhost/", self)).toBe(true);
    });

    it("leaves the daemon's other paths alone", () => {
      // Reading an API response in a pane is legitimate and is not a second app.
      expect(isVeldOwnUi("http://127.0.0.1:19899/api/health", self)).toBe(false);
      expect(isVeldOwnUi("https://veld.localhost/api/repos", self)).toBe(false);
      // `/ideas` is not `/ide`.
      expect(isVeldOwnUi("http://127.0.0.1:19899/ideas", self)).toBe(false);
    });

    it("never catches a dev server, which is the pane's whole job", () => {
      // The reason this is not "any loopback host": a project with its own `/ide`
      // route is not far-fetched, and `/` on a dev server is the commonest URL
      // there is.
      expect(isVeldOwnUi("http://localhost:3000/", self)).toBe(false);
      expect(isVeldOwnUi("http://localhost:3000/ide", self)).toBe(false);
      expect(isVeldOwnUi("http://127.0.0.1:3000/ide", self)).toBe(false);
      // A run's own services are subdomains, so they are a different hostname.
      expect(isVeldOwnUi("https://web.dev.veld.localhost/", self)).toBe(false);
      expect(isVeldOwnUi("https://veld-api.dev.veld.localhost/", self)).toBe(false);
    });

    it("refuses to judge what is not an http(s) URL", () => {
      expect(isVeldOwnUi("not a url", self)).toBe(false);
      expect(isVeldOwnUi("file:///ide", self)).toBe(false);
      // An empty self origin (no `location`, i.e. a test) must not make every
      // relative-looking thing match.
      expect(isVeldOwnUi("http://127.0.0.1:19899/ide", "")).toBe(false);
    });
  });

  it("labels a tab by host, falling back rather than throwing", () => {
    expect(urlLabel("https://web.dev.veld.localhost/a/b")).toBe("web.dev.veld.localhost");
    expect(urlLabel("http://localhost:3000")).toBe("localhost:3000");
    expect(urlLabel(undefined)).toBe("Browser");
    expect(urlLabel("not a url")).toBe("Browser");
  });

  it("normalises the seed URL and defaults the profile", () => {
    const tab = browserTab({ url: "localhost:3000" });
    expect(tab.kind).toBe("browser");
    expect(tab.url).toBe("https://localhost:3000/");
    expect(tab.profile).toBe("default");
    expect(tab.title).toBe("localhost:3000");

    // A refused scheme leaves a blank pane, never a seeded one.
    expect(browserTab({ url: "javascript:alert(1)" }).url).toBeUndefined();
    expect(browserTab({ url: "x.test", title: "web" }).title).toBe("web");
  });

  it("re-validates a restored URL and profile", () => {
    const restore = (tab: unknown) =>
      parseLayouts(
        JSON.stringify({
          1: {
            docks: [
              { tabs: [tab], activeId: "a" },
              { tabs: [], activeId: null },
            ],
            ratio: 0.5,
            focused: 0,
          },
        }),
      )[1]?.docks[0].tabs[0];

    // Storage is exactly where a stale build's — or a hand-edited — hostile URL
    // would sit waiting to be handed to a view on restore.
    expect(
      restore({ id: "a", kind: "browser", title: "t", url: "javascript:alert(1)" })?.url,
    ).toBeUndefined();
    expect(restore({ id: "a", kind: "browser", title: "t", url: "http://x.test/" })?.url).toBe(
      "http://x.test/",
    );
    // An unknown profile is a name the shell would refuse; fall back, don't drop
    // the tab.
    expect(restore({ id: "a", kind: "browser", title: "t", profile: "../etc" })?.profile).toBe(
      "default",
    );
    expect(restore({ id: "a", kind: "browser", title: "t", profile: "otter" })?.profile).toBe(
      "otter",
    );
    // A terminal tab gains no browser fields.
    expect(restore({ id: "a", kind: "terminal", title: "t", profile: "otter" })).toEqual({
      id: "a",
      kind: "terminal",
      title: "t",
    });
  });

  it("restores the emulated device and the zoom, re-validated", () => {
    const restore = (tab: unknown) =>
      parseLayouts(
        JSON.stringify({
          1: {
            docks: [
              { tabs: [tab], activeId: "a" },
              { tabs: [], activeId: null },
            ],
            ratio: 0.5,
            focused: 0,
          },
        }),
      )[1]?.docks[0].tabs[0];

    // The point of storing them at all: emulation and zoom are
    // per-`WebContents`, and the view is recreated on a session switch and on a
    // reload, so the layout is the only thing that outlives it.
    const emulation = {
      device: "phone",
      width: 402,
      height: 874,
      deviceScaleFactor: 3,
      mobile: true,
      touch: true,
      ua: "Mozilla/5.0 (iPhone) Safari/604.1",
      fit: true,
      radius: 44,
    };
    const restored = restore({ id: "a", kind: "browser", title: "t", emulation, zoom: 1.25 });
    expect(restored?.emulation).toEqual(emulation);
    expect(restored?.zoom).toBe(1.25);

    // Clamped and re-checked, exactly as the URL is: storage is where a stale
    // build's or a hand-edited value sits waiting to be applied. The user agent is
    // the one that matters — it becomes a request header in the shell.
    const hostile = restore({
      id: "a",
      kind: "browser",
      title: "t",
      emulation: { ...emulation, width: 99999, ua: "UA/1.0\r\nX-Injected: 1" },
      zoom: 99,
    });
    expect(hostile?.emulation?.width).toBe(4096);
    expect(hostile?.emulation?.ua).toBeNull();
    expect(hostile?.zoom).toBe(3);

    // Junk degrades to "no device, 100%" — a pane at pane size — rather than
    // dropping the tab or restoring a size nobody chose.
    const junk = restore({ id: "a", kind: "browser", title: "t", emulation: 42, zoom: "big" });
    expect(junk?.emulation).toBeUndefined();
    expect(junk?.zoom).toBeUndefined();

    // A tab written before this existed, and a pane switched back to pane size,
    // are the same record.
    const plain = restore({ id: "a", kind: "browser", title: "t", url: "http://x.test/" });
    expect(plain?.emulation).toBeUndefined();
    expect(plain?.zoom).toBeUndefined();
    // 100% is nothing to store, so it never comes back as a field either.
    expect(restore({ id: "a", kind: "browser", title: "t", zoom: 1 })?.zoom).toBeUndefined();

    // A terminal tab gains none of it, however the storage was edited.
    expect(restore({ id: "a", kind: "terminal", title: "t", emulation, zoom: 2 })).toEqual({
      id: "a",
      kind: "terminal",
      title: "t",
    });
  });

  it("lists browser ids separately from terminal ids", () => {
    let l = defaultLayout();
    // The default layout already holds one (the right-hand pane).
    const opened = browserIds(l);
    expect(opened).toHaveLength(1);
    const b = browserTab({ url: "http://x.test/" });
    l = addTab(l, 1, b);
    expect(browserIds(l)).toEqual([...opened, b.id]);
    expect(terminalIds(l)).not.toContain(b.id);
  });

  it("patches a tab, and returns the same layout when nothing changes", () => {
    let l = defaultLayout();
    const b = browserTab({ url: "http://x.test/" });
    l = addTab(l, 1, b);

    const moved = updateTab(l, b.id, { url: "http://y.test/", title: "y" });
    expect(findTab(moved, b.id)?.url).toBe("http://y.test/");
    expect(findTab(moved, b.id)?.title).toBe("y");

    // A `did-navigate` that re-reports the current URL must not re-render the
    // dock, so the identical patch is the same object.
    expect(updateTab(moved, b.id, { url: "http://y.test/" })).toBe(moved);
    expect(updateTab(moved, "nope", { url: "http://z.test/" })).toBe(moved);
  });
});

describe("single-dock normalisation", () => {
  it("never leaves the left dock empty while the right one has tabs", () => {
    // The invariant behind "Move to the left pane" ever making sense: with one
    // pane visible it is always dock 0.
    let l = defaultLayout();
    for (const id of l.docks[0].tabs.map((t) => t.id)) l = closeTab(l, id);
    expect(l.docks[0].tabs.length).toBeGreaterThan(0);
    expect(l.docks[1].tabs).toHaveLength(0);
  });

  it("restores a left-empty layout written by an older build", () => {
    const raw = JSON.stringify({
      1: {
        docks: [
          { tabs: [], activeId: null },
          { tabs: [{ id: "a", kind: "terminal", title: "t" }], activeId: "a" },
        ],
        ratio: 0.5,
        focused: 1,
      },
    });
    const l = parseLayouts(raw)[1];
    expect(l.docks[0].tabs.map((t) => t.id)).toEqual(["a"]);
    expect(l.docks[1].tabs).toHaveLength(0);
    expect(l.focused).toBe(0);
  });

  it("leaves a two-dock layout alone", () => {
    const l = defaultLayout();
    expect(dockVisible(l, 0)).toBe(true);
    expect(dockVisible(l, 1)).toBe(true);
    expect(closeTab(l, "nope")).toBe(l);
  });
});

describe("browser sessions", () => {
  it("gives every slot but the default a distinct colour", () => {
    expect(BROWSER_PROFILE_COLORS.default).toBeNull();
    const colors = BROWSER_PROFILES.filter((p) => p !== "default").map(
      (p) => BROWSER_PROFILE_COLORS[p],
    );
    expect(colors).toHaveLength(MAX_EXTRA_SESSIONS);
    expect(colors.every((c) => typeof c === "string")).toBe(true);
    // A repeated colour would make two sessions indistinguishable, which is the
    // one job the colour has.
    expect(new Set(colors).size).toBe(colors.length);
    // Every slot is a legal Electron partition name (`PROFILE_RE` in
    // desktop/src/browserViews.js), or the shell rejects it at create time.
    for (const p of BROWSER_PROFILES) expect(p).toMatch(/^[a-z0-9][a-z0-9-]{0,31}$/);
  });

  it("labels slots by name, since a number implies a missing predecessor", () => {
    expect(browserProfileLabel("default")).toBe("Default");
    expect(browserProfileLabel("narwhal")).toBe("Narwhal");
  });

  it("reports which slots are occupied, across every layout", () => {
    let a = defaultLayout();
    // The default layout's own browser pane sits on the default session.
    expect(sessionsInUse([a])).toEqual(new Set(["default"]));
    a = addTab(a, 0, browserTab({ url: "http://a.test/", profile: "wombat" }));
    expect(sessionsInUse([a])).toEqual(new Set(["wombat", "default"]));

    // A pane in a worktree the user has switched away from still holds its jar,
    // so the union is what "exists" means.
    const b = addTab(defaultLayout(), 0, browserTab({ profile: "badger" }));
    expect(sessionsInUse([a, b])).toEqual(new Set(["wombat", "default", "badger"]));
  });

  it("hands out the lowest free slot, and nothing once they are all taken", () => {
    expect(nextFreeProfile(new Set())).toBe("otter");
    // Lowest, not next-after-highest: closing the pane on session 2 frees its
    // colour for reuse rather than marching towards the cap.
    expect(nextFreeProfile(new Set(["wombat", "gecko"]))).toBe("otter");
    expect(nextFreeProfile(new Set(["default", "otter"]))).toBe("wombat");
    // The default slot never gets handed out as a "new" session.
    const all = new Set(BROWSER_PROFILES.filter((p) => p !== "default"));
    expect(nextFreeProfile(all)).toBeNull();
  });
});

describe("session sets", () => {
  it("always contains the default slot, deduped and in slot order", () => {
    // Slot order, not insertion order: a session's colour is tied to its slot, so
    // a list that reshuffled itself would make the colours look arbitrary.
    expect(normalizeSessionSet([])).toEqual(["default"]);
    expect(normalizeSessionSet(["badger", "otter", "badger"])).toEqual([
      "default",
      "otter",
      "badger",
    ]);
    expect(normalizeSessionSet(["default"])).toEqual(["default"]);
  });

  it("round-trips, and survives storage that is not what we wrote", () => {
    const sets = { 3: ["default", "otter"] as BrowserProfile[] };
    expect(parseSessionSets(serializeSessionSets(sets))).toEqual(sets);

    expect(parseSessionSets(null)).toEqual({});
    expect(parseSessionSets("")).toEqual({});
    expect(parseSessionSets("not json")).toEqual({});
    expect(parseSessionSets("[]")).toEqual({});
    expect(parseSessionSets('{"1":"nope"}')).toEqual({});
    // A slot name outside the allowed set becomes an Electron partition if it
    // gets through, so it is dropped rather than carried.
    expect(parseSessionSets('{"1":["otter","../etc","session-2"]}')).toEqual({
      1: ["default", "otter"],
    });
    // Non-numeric keys can't index a worktree.
    expect(Object.keys(parseSessionSets('{"abc":["otter"],"4":["otter"]}'))).toEqual([
      "4",
    ]);
  });

  it("unions the stored set with what the layout actually uses", () => {
    // A layout can name a session the stored set has lost (storage cleared, an
    // older build). A pane missing from its own menu is worse than an extra row.
    const layout = addTab(defaultLayout(), 0, browserTab({ profile: "puffin" }));
    expect(sessionSetFor({}, 1, layout)).toEqual(["default", "puffin"]);
    expect(sessionSetFor({ 1: ["default", "otter"] }, 1, layout)).toEqual([
      "default",
      "otter",
      "puffin",
    ]);
    // Per worktree: another worktree's set is not this one's.
    expect(sessionSetFor({ 2: ["default", "otter"] }, 1)).toEqual(["default"]);
  });

  it("adding a session keeps the ones already there", () => {
    // The bug this replaced: the set was derived from occupancy, so moving a pane
    // onto a new slot vacated its old one and adding looked like deleting.
    let set = normalizeSessionSet([]);
    const first = nextFreeProfile(new Set(set));
    set = normalizeSessionSet([...set, first!]);
    const second = nextFreeProfile(new Set(set));
    set = normalizeSessionSet([...set, second!]);
    expect(set).toEqual(["default", "otter", "wombat"]);
    expect(first).not.toBe(second);
  });
});

describe("replaceTab", () => {
  it("swaps content in place, keeping position and active state", () => {
    let l = defaultLayout();
    const first = l.docks[0].tabs[0];
    const placeholder = newPaneTab();
    l = addTab(l, 0, placeholder);
    const chosen = browserTab({ url: "http://x.test/" });

    l = replaceTab(l, placeholder.id, chosen);
    expect(ids(l, 0)).toEqual([first.id, chosen.id]);
    expect(l.docks[0].activeId).toBe(chosen.id);
    expect(l.focused).toBe(0);
    // The placeholder's id is gone — it must not linger as a second tab.
    expect(findTab(l, placeholder.id)).toBeNull();
  });

  it("leaves a non-active tab's position and the dock's active tab alone", () => {
    let l = defaultLayout();
    const placeholder = newPaneTab();
    l = addTab(l, 0, placeholder);
    const first = l.docks[0].tabs[0];
    l = activateTab(l, first.id);
    const chosen = browserTab({});
    l = replaceTab(l, placeholder.id, chosen);
    expect(ids(l, 0)).toEqual([first.id, chosen.id]);
    expect(l.docks[0].activeId).toBe(first.id);
  });

  it("refuses to create a second tab with an existing id", () => {
    // Two tabs sharing an id would fight over one live terminal or one live view.
    let l = defaultLayout();
    const existing = l.docks[0].tabs[0];
    const placeholder = newPaneTab();
    l = addTab(l, 0, placeholder);
    const after = replaceTab(l, placeholder.id, { ...existing });
    expect(allTabs(after).filter((t) => t.id === existing.id)).toHaveLength(1);
    expect(findTab(after, placeholder.id)).not.toBeNull();
  });

  it("is a no-op for an unknown id", () => {
    const l = defaultLayout();
    expect(replaceTab(l, "nope", browserTab({}))).toBe(l);
  });

  it("survives a reload as a `new` pane", () => {
    // `new` has to be in PANE_KINDS or the restore validator drops the tab and
    // the pane vanishes on the first reload.
    const l = addTab(defaultLayout(), 0, newPaneTab());
    const back = parseLayouts(serializeLayouts({ 1: l }))[1];
    expect(back.docks[0].tabs.some((t) => t.kind === "new")).toBe(true);
  });
});

describe("paneTabLabel", () => {
  it("names every kind from the kind, not from stored state", () => {
    // A layout written by an older build carries whatever title that build used,
    // so reading `tab.title` would leave a renamed kind mislabelled after a
    // reload. Only a browser pane's title is its own (the page's).
    let l = defaultLayout();
    const terminal = term();
    l = addTab(l, 0, terminal);
    expect(paneTabLabel(l, terminal)).toBe("Terminal");
    expect(paneTabLabel(l, { ...terminal, title: "stale" })).toBe("Terminal");

    expect(paneTabLabel(l, newPaneTab())).toBe("New pane");
    expect(paneTabLabel(l, diagTab("logs"))).toBe("Logs");
    expect(paneTabLabel(l, diagTab("nodes"))).toBe("Nodes");
    expect(paneTabLabel(l, { ...diagTab("logs"), title: "stale" })).toBe("Logs");
  });

  it("uses a browser pane's page title, falling back to its host", () => {
    const l = defaultLayout();
    const withTitle = browserTab({ url: "http://x.test/", title: "Veld" });
    expect(paneTabLabel(l, withTitle)).toBe("Veld");
    expect(paneTabLabel(l, { ...withTitle, title: "" })).toBe("x.test");
    expect(paneTabLabel(l, { ...withTitle, title: "", url: undefined })).toBe("Browser");
  });
});

describe("lastBlankBrowserId", () => {
  it("finds the last browser pane with nothing loaded", () => {
    // What the top bar's globe asks: is a pane already showing the URL list?
    let l = defaultLayout();
    const seeded = l.docks[1].tabs[0].id;
    expect(lastBlankBrowserId(l)).toBe(seeded);

    // Once it has a URL it is no longer showing the list.
    l = updateTab(l, seeded, { url: "http://x.test/" });
    expect(lastBlankBrowserId(l)).toBeNull();

    // The *last* one, so asking twice lands in the same pane rather than cycling.
    const a = browserTab({});
    const b = browserTab({});
    l = addTab(l, 0, a);
    l = addTab(l, 1, b);
    expect(lastBlankBrowserId(l)).toBe(b.id);
  });

  it("ignores terminals and undecided panes", () => {
    let l = defaultLayout();
    l = updateTab(l, l.docks[1].tabs[0].id, { url: "http://x.test/" });
    l = addTab(l, 0, newPaneTab());
    expect(lastBlankBrowserId(l)).toBeNull();
  });
});

describe("restoring a layout from the pre-branch build", () => {
  it("drops only the removed kind, keeping the terminal and its session id", () => {
    // The `services` pane kind was deleted (its content is now a launcher shown
    // inside other panes), so a layout persisted by an older build names a kind
    // `PANE_KINDS` no longer contains. That must degrade, not throw and not take
    // the dock with it — and the terminal's id is its daemon PTY session, so
    // losing it would strand a running shell on upgrade.
    const legacy = JSON.stringify({
      1: {
        docks: [
          { tabs: [{ id: "t-abc", kind: "terminal", title: "terminal" }], activeId: "t-abc" },
          { tabs: [{ id: "services", kind: "services", title: "services" }], activeId: "services" },
        ],
        ratio: 0.5,
        focused: 1,
      },
    });
    const l = parseLayouts(legacy)[1];
    expect(l).toBeDefined();
    expect(l.docks[0].tabs.map((t) => t.id)).toEqual(["t-abc"]);
    expect(l.docks[0].activeId).toBe("t-abc");
    expect(l.docks[1].tabs).toEqual([]);
    // `focused` pointed at the dock that is now empty, and normalisation slid the
    // survivor left, so it has to follow rather than aim at a hidden column.
    expect(l.focused).toBe(0);
  });

  it("drops a layout whose only tab was the removed kind", () => {
    // Nothing left to render: the worktree falls back to a fresh default layout
    // rather than restoring an empty dock pair.
    const legacy = JSON.stringify({
      1: {
        docks: [
          { tabs: [{ id: "services", kind: "services", title: "services" }], activeId: "services" },
          { tabs: [], activeId: null },
        ],
        ratio: 0.5,
        focused: 0,
      },
    });
    expect(parseLayouts(legacy)).toEqual({});
  });
});

describe("layout slots", () => {
  /** A `LayoutStorage` backed by a plain map. */
  function fake(initial: Record<string, string> = {}) {
    const map = new Map(Object.entries(initial));
    return {
      getItem: (k: string) => map.get(k) ?? null,
      setItem: (k: string, v: string) => {
        map.set(k, v);
      },
      map,
    };
  }

  const layout = defaultLayout(DEFAULT_RATIO);
  const layouts = { 7: layout };

  it("writes a main window's layouts to the shared per-worktree store", () => {
    const session = fake();
    const durable = fake();
    writeLayouts(session, durable, "main", layouts);
    expect(session.map.get("veld.panes.v1")).toBe(serializeLayouts(layouts));
    // Keyed by worktree and shared, not by window: a worktree has one set of
    // panes, and whichever window shows it next picks that set up.
    expect(durable.map.get(LAYOUT_WORKTREE_KEY)).toBe(serializeLayouts(layouts));
    expect(durable.map.get(layoutSlotKey("main"))).toBeUndefined();
  });

  it("merges the shared store rather than replacing it", () => {
    // Each window holds only the worktree it is showing, so a blind write would
    // delete every other window's worktree from the shared key. Same hazard and
    // same fix as `editSessions` — read through at write time, not at boot.
    const durable = fake({
      [LAYOUT_WORKTREE_KEY]: serializeLayouts({ 4: defaultLayout(0.2), 7: defaultLayout(0.9) }),
    });
    writeLayouts(fake(), durable, "main", { 7: layout });
    const written = parseLayouts(durable.map.get(LAYOUT_WORKTREE_KEY) ?? null);
    expect(Object.keys(written).sort()).toEqual(["4", "7"]);
    expect(written[7].ratio).toBe(layout.ratio);
    // …and the other window's worktree survives untouched.
    expect(written[4].ratio).toBe(0.2);
  });

  describe("dropWorktreeLayouts", () => {
    it("removes a deleted worktree the merge would otherwise keep forever", () => {
      // The merge above is exactly why this has to exist: dropping a worktree from
      // the app's own `layouts` leaves the stored copy in place, and worktree
      // rowids are reused — so the *next* worktree created inherits a deleted
      // one's terminals and browser panes.
      const durable = fake({
        [LAYOUT_WORKTREE_KEY]: serializeLayouts({ 4: defaultLayout(0.2), 7: layout }),
      });
      dropWorktreeLayouts(durable, [7]);
      const written = parseLayouts(durable.map.get(LAYOUT_WORKTREE_KEY) ?? null);
      expect(Object.keys(written)).toEqual(["4"]);
      expect(written[4].ratio).toBe(0.2);
    });

    it("does not rewrite the shared key when it changed nothing", () => {
      // Two windows share this key, so a pointless write is a chance to clobber a
      // concurrent one for no gain.
      const stored = serializeLayouts({ 4: defaultLayout(0.2) });
      const durable = fake({ [LAYOUT_WORKTREE_KEY]: stored });
      let writes = 0;
      const counted = {
        getItem: durable.getItem,
        setItem: (k: string, v: string) => {
          writes += 1;
          durable.setItem(k, v);
        },
      };
      dropWorktreeLayouts(counted, [99]);
      dropWorktreeLayouts(counted, []);
      expect(writes).toBe(0);
      expect(durable.map.get(LAYOUT_WORKTREE_KEY)).toBe(stored);
    });

    it("tolerates no durable storage at all", () => {
      // Same configurations `storages()` guards against; forgetting is not a
      // reason to throw where saving would not.
      expect(() => dropWorktreeLayouts(null, [7])).not.toThrow();
    });
  });

  it("keeps a detached window out of the shared store", () => {
    // A satellite's tabs were transferred *out of* a worktree a main window
    // owns, so writing them under that worktree's key would overwrite the
    // layout they came from.
    const durable = fake();
    writeLayouts(fake(), durable, "main-w2", layouts, true);
    expect(durable.map.get(layoutSlotKey("main-w2"))).toBe(serializeLayouts(layouts));
    expect(durable.map.get(LAYOUT_WORKTREE_KEY)).toBeUndefined();
  });

  it("writes only the session store without a slot", () => {
    const session = fake();
    const durable = fake();
    writeLayouts(session, durable, null, layouts);
    expect(session.map.size).toBe(1);
    // A browser tab must not leave a layout behind that another tab could
    // restore — both would attach to the same shells and take them from each
    // other on every reattach.
    expect(durable.map.size).toBe(0);
  });

  it("restores a *satellite* from the slot store when the session store is empty", () => {
    // The app restarted: a new window, so a new sessionStorage, but the holder
    // processes kept the shells running and their ids are in here. A main
    // window picks its worktrees up one at a time instead — see "gives a main
    // window nothing at boot" below.
    const durable = fake({ [layoutSlotKey("main-w2")]: serializeLayouts(layouts) });
    expect(readLayouts(fake(), durable, "main-w2", null, true, true)).toEqual(layouts);
  });

  it("prefers the session store, which is this window's own state", () => {
    const mine = { 9: defaultLayout(0.3) };
    const stale = { 7: layout };
    const session = fake({ "veld.panes.v1": serializeLayouts(mine) });
    const durable = fake({ [layoutSlotKey("main")]: serializeLayouts(stale) });
    // A reload must not resurrect a layout from before the last change.
    expect(readLayouts(session, durable, "main")).toEqual(mine);
  });

  it("ignores the slot store without a slot", () => {
    const durable = fake({ [layoutSlotKey("main")]: serializeLayouts(layouts) });
    expect(readLayouts(fake(), durable, null)).toEqual({});
  });

  it("keeps satellites apart, by slot", () => {
    const durable = fake();
    writeLayouts(fake(), durable, "main-w2", layouts, true);
    // A detached window restores only its *own* slot. Recycled slots are why
    // this also needs `restored`: see "only a reopened window reads the slot
    // store" below.
    expect(readLayouts(fake(), durable, "main-w3", null, true, true)).toEqual({});
    expect(readLayouts(fake(), durable, "main-w2", null, true, true)).toEqual(layouts);
    expect(layoutSlotKey("main-w2")).not.toBe(layoutSlotKey("main-w3"));
  });

  it("hands one worktree's panes to whichever main window shows it next", () => {
    // The point of the shared store. Window A writes its layouts; window B, on
    // a different slot, picks the same worktree up — one set of panes, one
    // window showing them, and no hand-off protocol between them. What stops
    // *both* rendering it is the shell's claim, not this key.
    const durable = fake();
    writeLayouts(fake(), durable, "main", { 7: layout });
    expect(worktreeLayoutFrom(durable, 7)).toEqual(layout);
  });

  it("gives a main window nothing at boot, whatever the store holds", () => {
    // Ownership, not thrift. `writeLayouts` merges this window's `layouts` over
    // what is on disk, so a window that booted holding every worktree stamped
    // its boot snapshot back over each of them on every save — reverting
    // worktrees another window had been editing since, and orphaning the panes
    // added in the meantime. A window owns what it displays and picks the rest
    // up one at a time through `worktreeLayoutFrom`.
    const durable = fake({ [LAYOUT_WORKTREE_KEY]: serializeLayouts(layouts) });
    expect(readLayouts(fake(), durable, "main")).toEqual({});
    expect(readLayouts(fake(), durable, "main", null, true)).toEqual({});
  });

  it("reads one worktree's panes fresh, not from a boot snapshot", () => {
    // What stops a window that claims a worktree from inventing a second set:
    // it may not have that worktree in memory *because another window has been
    // using it since this one booted*, and those panes are the ones that exist.
    const durable = fake();
    writeLayouts(fake(), durable, "main", { 7: layout });
    expect(worktreeLayoutFrom(durable, 7)).toEqual(layout);
    expect(worktreeLayoutFrom(durable, 99)).toBeNull();
  });

  it("gives a plain browser tab nothing durable, in either direction", () => {
    // A tab is a session; two of them restoring one layout would attach to the
    // same shells and take them from each other on every reattach.
    const durable = fake({ [LAYOUT_WORKTREE_KEY]: serializeLayouts(layouts) });
    expect(readLayouts(fake(), durable, null)).toEqual({});
    writeLayouts(fake(), durable, null, { 9: defaultLayout(0.4) });
    expect(parseLayouts(durable.map.get(LAYOUT_WORKTREE_KEY) ?? null)).toEqual(layouts);
  });

  it("survives storage being unavailable", () => {
    // Storage access throws outright in some privacy configurations, and this
    // runs in a useState initialiser where a throw white-screens the app.
    expect(readLayouts(null, null, "main")).toEqual({});
    expect(() => writeLayouts(null, null, "main", layouts)).not.toThrow();
  });

  it("degrades to no saved layout on a corrupt slot store", () => {
    const durable = fake({ [layoutSlotKey("main")]: "{not json" });
    expect(readLayouts(fake(), durable, "main")).toEqual({});
  });

  describe("the detach seed", () => {
    const seeded = { 4: defaultLayout(0.25) };
    const seed = serializeLayouts(seeded);

    it("is what a brand-new detached window boots with", () => {
      // Neither store has anything: this window did not exist a moment ago, and
      // the tabs it is meant to hold were handed to it on its command line.
      expect(readLayouts(fake(), fake(), "main-w2", seed)).toEqual(seeded);
    });

    it("loses to the session store, so a reload does not resurrect it", () => {
      const mine = { 4: defaultLayout(0.8) };
      expect(
        readLayouts(fake({ "veld.panes.v1": serializeLayouts(mine) }), fake(), "main-w2", seed),
      ).toEqual(mine);
    });

    it("BEATS the slot store, because slots are reused", () => {
      // The regression this ordering exists for. `nextSuffix` counts live
      // windows only and nothing clears a slot's key, so: detach (window takes
      // `main-w2`, writes its layout) → close it (tabs handed back, ids now live
      // in the origin) → detach again → the new window lands on `main-w2` and
      // finds the *dead* layout sitting there.
      //
      // Reading it would discard the seed, which is silent twice over: the tab
      // being moved exists in no layout at all (the origin already released and
      // closed it) so its shell dies at the grace, and the resurrected ids get
      // attached to, taking them over from the window that just adopted them.
      const dead = { 4: defaultLayout(0.8) };
      const durable = fake({ [layoutSlotKey("main-w2")]: serializeLayouts(dead) });
      expect(readLayouts(fake(), durable, "main-w2", seed)).toEqual(seeded);
    });

    it("is not consulted by a restored window, which has no seed", () => {
      // The case the wrong ordering was written for, and it cannot arise: a
      // restored window is opened with a slot and no seed at all.
      const own = { 4: defaultLayout(0.8) };
      const durable = fake({ [layoutSlotKey("main-w2")]: serializeLayouts(own) });
      expect(readLayouts(fake(), durable, "main-w2", null, true, true)).toEqual(own);
    });

    it("goes through the same validation a restored layout does", () => {
      // The seed has been out of the page and through the main process. A
      // `javascript:` URL reaching a view from here is no better than one
      // reaching it from storage.
      const hostile = JSON.stringify({
        4: {
          docks: [
            { tabs: [{ id: "v1", kind: "browser", url: "javascript:alert(1)" }], activeId: "v1" },
            { tabs: [], activeId: null },
          ],
          ratio: 0.5,
          focused: 0,
        },
      });
      const restored = readLayouts(fake(), fake(), "main-w2", hostile);
      expect(restored[4].docks[0].tabs[0].url).toBeUndefined();
    });

    it("is absent by default, so nothing changes for a window that has none", () => {
      expect(readLayouts(fake(), fake(), "main")).toEqual({});
      expect(readLayouts(fake(), fake(), "main", null)).toEqual({});
      expect(readLayouts(fake(), fake(), "main", "{not json")).toEqual({});
    });
  });

  describe("only a reopened window reads the slot store", () => {
    const stored = { 4: defaultLayout(0.8) };
    const durable = () => fake({ [layoutSlotKey("main-w2")]: serializeLayouts(stored) });

    it("gives a brand-new window nothing, however full the slot is", () => {
      // ⌘N is the case, and the seed fix did not cover it. Suffixes are
      // recycled and a slot's key is never cleared, so to a genuinely new
      // window that layout is a *dead* one that happens to share a number —
      // adopting it means attaching to terminal ids another window is using,
      // and an attach takes a shell over.
      //
      // Full sequence: detach a terminal, close the detached window (its tabs
      // go back to the origin, which re-attaches), press ⌘N. The new window
      // gets the freed suffix, restored the dead layout naming that same tab
      // id, and stole the shell — leaving the origin on "connection lost",
      // which is the exact ping-pong slots exist to prevent.
      expect(readLayouts(fake(), durable(), "main-w2", null, false, true)).toEqual({});
    });

    it("gives a reopened satellite its layout back", () => {
      // The other half, and the whole point of the durable store: shells that
      // outlived the app have to be reachable again.
      expect(readLayouts(fake(), durable(), "main-w2", null, true, true)).toEqual(stored);
    });

    it("defaults to not restoring, so a new caller cannot opt in by accident", () => {
      expect(readLayouts(fake(), durable(), "main-w2", null, undefined, true)).toEqual({});
    });

    it("does not gate the session store or the seed", () => {
      // A reload of a new window still has to come back, and a detached window
      // still has to boot with what it was handed.
      const mine = { 4: defaultLayout(0.3) };
      expect(
        readLayouts(
          fake({ "veld.panes.v1": serializeLayouts(mine) }),
          durable(),
          "main-w2",
          null,
          false,
          true,
        ),
      ).toEqual(mine);
      const seeded = { 5: defaultLayout(0.25) };
      expect(
        readLayouts(fake(), durable(), "main-w2", serializeLayouts(seeded), false, true),
      ).toEqual(seeded);
    });
  });
});

describe("splitWithTab", () => {
  const tab = (id: string): PaneTab => ({ id, kind: "new", title: id });

  function oneDock(...ids: string[]): PaneLayout {
    return {
      docks: [
        { tabs: ids.map(tab), activeId: ids[0] ?? null },
        { tabs: [], activeId: null },
      ],
      ratio: DEFAULT_RATIO,
      focused: 0,
    };
  }

  it("creates the second dock on the right", () => {
    const next = splitWithTab(oneDock("a", "b"), "b", 1);
    expect(next.docks[0].tabs.map((t) => t.id)).toEqual(["a"]);
    expect(next.docks[1].tabs.map((t) => t.id)).toEqual(["b"]);
    expect(next.focused).toBe(1);
  });

  it("creates the second dock on the left, moving everything else right", () => {
    // The half `moveTab` cannot express: there is no dock index that means
    // "become the left pane and push the current one to the right".
    const next = splitWithTab(oneDock("a", "b", "c"), "c", 0);
    expect(next.docks[0].tabs.map((t) => t.id)).toEqual(["c"]);
    expect(next.docks[1].tabs.map((t) => t.id)).toEqual(["a", "b"]);
    expect(next.docks[0].activeId).toBe("c");
    expect(next.focused).toBe(0);
  });

  it("keeps an active tab on the side it did not move to", () => {
    const layout = oneDock("a", "b", "c");
    // `a` is active and stays behind; the successor rule applies to the dock it
    // was left in, not to the one being created.
    const next = splitWithTab(layout, "c", 0);
    expect(next.docks[1].activeId).toBe("a");
  });

  it("is a no-op for a dock's only tab", () => {
    // With one pane on screen the sides mean nothing — the same rule
    // `normalizeDocks` enforces everywhere else.
    const layout = oneDock("a");
    expect(splitWithTab(layout, "a", 0)).toBe(layout);
    expect(splitWithTab(layout, "a", 1)).toBe(layout);
  });

  it("is a plain move once both docks are on screen", () => {
    const layout = splitWithTab(oneDock("a", "b"), "b", 1);
    const back = splitWithTab(layout, "b", 0);
    expect(back.docks[0].tabs.map((t) => t.id)).toEqual(["a", "b"]);
    // Emptying the right dock slides nothing: it was the right one.
    expect(back.docks[1].tabs).toEqual([]);
  });

  it("ignores an id that is not in the layout", () => {
    const layout = oneDock("a", "b");
    expect(splitWithTab(layout, "ghost", 1)).toBe(layout);
  });
});

describe("insertTab", () => {
  const tab = (id: string): PaneTab => ({ id, kind: "new", title: id });
  const three = () => {
    let l: PaneLayout = {
      docks: [
        { tabs: [tab("a")], activeId: "a" },
        { tabs: [], activeId: null },
      ],
      ratio: DEFAULT_RATIO,
      focused: 0,
    };
    l = insertTab(l, 0, tab("b"));
    l = insertTab(l, 0, tab("c"));
    return l;
  };

  it("places a tab where the caret was, not at the end", () => {
    // What makes a cross-window drop honour where you aimed. `addTab` appends,
    // which is right for a tab this window created and wrong for one arriving
    // from another with a position behind it.
    const l = insertTab(three(), 0, tab("x"), 1);
    expect(l.docks[0].tabs.map((t) => t.id)).toEqual(["a", "x", "b", "c"]);
    expect(l.docks[0].activeId).toBe("x");
    expect(l.focused).toBe(0);
  });

  it("appends with no index, and clamps a nonsense one", () => {
    expect(insertTab(three(), 0, tab("x")).docks[0].tabs.map((t) => t.id)).toEqual([
      "a",
      "b",
      "c",
      "x",
    ]);
    expect(insertTab(three(), 0, tab("x"), 99).docks[0].tabs.at(-1)?.id).toBe("x");
    expect(insertTab(three(), 0, tab("x"), -5).docks[0].tabs[0].id).toBe("x");
  });

  it("opens the second dock", () => {
    const l = insertTab(three(), 1, tab("x"));
    expect(l.docks[1].tabs.map((t) => t.id)).toEqual(["x"]);
    expect(l.focused).toBe(1);
  });

  it("activates rather than duplicating an id it already holds", () => {
    // Two tabs on one id would fight over one shell — the same rule `addTab`
    // enforces, and now reachable from a second window too.
    const l = three();
    const again = insertTab(l, 1, tab("b"), 0);
    expect(allTabs(again).filter((t) => t.id === "b")).toHaveLength(1);
    expect(again.docks[1].tabs).toEqual([]);
  });
});

describe("adoptTabs", () => {
  const tab = (id: string): PaneTab => ({ id, kind: "terminal", title: id });

  it("builds a layout for a worktree this window has never opened", () => {
    const next = adoptTabs(undefined, [tab("a"), tab("b")]);
    expect(next?.docks[0].tabs.map((t) => t.id)).toEqual(["a", "b"]);
    expect(next?.docks[0].activeId).toBe("a");
    expect(next?.docks[1].tabs).toEqual([]);
  });

  it("appends to the focused dock of an existing layout", () => {
    const base = splitWithTab(
      {
        docks: [
          { tabs: [tab("a"), tab("b")], activeId: "a" },
          { tabs: [], activeId: null },
        ],
        ratio: DEFAULT_RATIO,
        focused: 0,
      },
      "b",
      1,
    );
    expect(base.focused).toBe(1);
    const next = adoptTabs(base, [tab("c")]);
    expect(next?.docks[1].tabs.map((t) => t.id)).toEqual(["b", "c"]);
  });

  it("skips ids the layout already holds", () => {
    const base = adoptTabs(undefined, [tab("a")]);
    // Two tabs on one id would fight over one shell. A hand-back racing a
    // detach that was never committed is exactly how that would arise.
    expect(adoptTabs(base ?? undefined, [tab("a")])).toBeNull();
    const mixed = adoptTabs(base ?? undefined, [tab("a"), tab("b")]);
    expect(mixed?.docks[0].tabs.map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("returns null rather than an unchanged layout for nothing to adopt", () => {
    expect(adoptTabs(undefined, [])).toBeNull();
  });
});

describe("parseTransferTabs", () => {
  it("applies the same gate a restored layout goes through", () => {
    const tabs = parseTransferTabs([
      { id: "sh1", kind: "terminal", title: "Terminal" },
      // A URL that has been out of the page and back is exactly as untrusted as
      // one read out of storage.
      { id: "v1", kind: "browser", title: "Bad", url: "javascript:alert(1)" },
      { id: "bad id", kind: "terminal" },
      { id: "k1", kind: "wormhole" },
      null,
    ]);
    expect(tabs.map((t) => t.id)).toEqual(["sh1", "v1"]);
    expect(tabs[1].url).toBeUndefined();
  });

  it("deduplicates and tolerates a non-list", () => {
    expect(
      parseTransferTabs([
        { id: "a", kind: "new", title: "one" },
        { id: "a", kind: "new", title: "two" },
      ]).length,
    ).toBe(1);
    expect(parseTransferTabs("nope")).toEqual([]);
    expect(parseTransferTabs(undefined)).toEqual([]);
  });
});

describe("revealDiagPane", () => {
  it("adds a pane when none is open", () => {
    const l = defaultLayout();
    const next = revealDiagPane(l, "nodes");
    const added = allTabs(next).filter((t) => t.kind === "nodes");
    expect(added.length).toBe(1);
    expect(next.docks[next.focused].activeId).toBe(added[0].id);
  });

  it("focuses the one already open instead of adding a second", () => {
    const existing = diagTab("nodes");
    const l = addTabToFocused(defaultLayout(), existing);
    // Something else on top, so "already open" is not the same as "already
    // visible" — the affordance has to bring it forward.
    const covered = addTabToFocused(l, newPaneTab());
    expect(covered.docks[covered.focused].activeId).not.toBe(existing.id);

    const next = revealDiagPane(covered, "nodes");
    expect(allTabs(next).filter((t) => t.kind === "nodes").length).toBe(1);
    expect(next.docks[next.focused].activeId).toBe(existing.id);
  });

  it("crosses docks rather than duplicating", () => {
    const existing = diagTab("nodes");
    const split = splitWithTab(addTabToFocused(defaultLayout(), existing), existing.id, 1);
    const other = addTab(split, 0, newPaneTab());
    const next = revealDiagPane(other, "nodes");
    expect(allTabs(next).filter((t) => t.kind === "nodes").length).toBe(1);
    // Focus follows the tab into the dock that holds it.
    expect(next.docks[next.focused].activeId).toBe(existing.id);
  });

  it("keeps logs and nodes independent", () => {
    const withLogs = revealDiagPane(defaultLayout(), "logs");
    const both = revealDiagPane(withLogs, "nodes");
    expect(allTabs(both).filter((t) => t.kind === "logs").length).toBe(1);
    expect(allTabs(both).filter((t) => t.kind === "nodes").length).toBe(1);
  });
});
