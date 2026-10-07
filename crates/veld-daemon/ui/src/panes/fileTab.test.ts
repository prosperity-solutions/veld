import { describe, expect, it } from "vitest";

import {
  type PaneLayout,
  type PaneTab,
  besideDock,
  browserTab,
  fileTab,
  fileTabIds,
  findBrowserTab,
  findFileTab,
  openBrowserTab,
  openFileTab,
  paneTabLabel,
  parseLayout,
  placeTab,
} from "./model";

const term = (id: string): PaneTab => ({ id, kind: "terminal", title: "Terminal" });

function layout(left: PaneTab[], right: PaneTab[] = [], focused: 0 | 1 = 0): PaneLayout {
  return {
    docks: [
      { tabs: left, activeId: left[0]?.id ?? null },
      { tabs: right, activeId: right[0]?.id ?? null },
    ],
    ratio: 0.5,
    focused,
  };
}

describe("file tabs", () => {
  it("are named by the file, and carry its path and line", () => {
    const tab = fileTab({ path: "./docs/plan.md", line: 12 });
    expect(tab).toMatchObject({ kind: "file", path: "docs/plan.md", line: 12, title: "plan.md" });
    expect(paneTabLabel(layout([tab]), tab)).toBe("plan.md");
    expect(fileTab({ path: "a.md", line: 0 }).line).toBeUndefined();
  });

  it("survive a reload, and a tab with no file does not", () => {
    const tab = fileTab({ path: "docs/plan.md", line: 3 });
    const parsed = parseLayout(JSON.parse(JSON.stringify(layout([term("t1"), tab]))));
    expect(parsed?.docks[0].tabs[1]).toEqual(tab);
    const broken = parseLayout({
      docks: [{ tabs: [term("t1"), { id: "f1", kind: "file", title: "x" }] }, { tabs: [] }],
    });
    expect(broken?.docks[0].tabs.map((t) => t.id)).toEqual(["t1"]);
    const control = parseLayout({
      docks: [{ tabs: [term("t1"), { id: "f2", kind: "file", path: "a\u0007.md" }] }, { tabs: [] }],
    });
    expect(control?.docks[0].tabs.map((t) => t.id)).toEqual(["t1"]);
  });

  it("open beside the terminal they came from, splitting a single dock", () => {
    const l = layout([term("t1")]);
    expect(besideDock(l, "t1")).toBe(1);
    const { layout: next, tabId } = openFileTab(l, {
      path: "plan.md",
      beside: besideDock(l, "t1"),
      focus: false,
    });
    expect(next.docks[1].tabs.map((t) => t.id)).toEqual([tabId]);
    expect(next.docks[1].activeId).toBe(tabId);
    // A push never takes the keyboard from the terminal.
    expect(next.focused).toBe(0);
    expect(fileTabIds(next)).toEqual([tabId]);
  });

  it("reuse the tab already showing the file, updating its line", () => {
    const first = openFileTab(layout([term("t1")]), { path: "plan.md", beside: 1, focus: false });
    const again = openFileTab(first.layout, {
      path: "./plan.md",
      line: 40,
      beside: 1,
      focus: true,
    });
    expect(again.tabId).toBe(first.tabId);
    expect(findFileTab(again.layout, "plan.md")?.line).toBe(40);
    expect(again.layout.docks[1].tabs).toHaveLength(1);
    expect(again.layout.focused).toBe(1);
    // No line means "show it", not "scroll to the top".
    const third = openFileTab(again.layout, { path: "plan.md", beside: 1, focus: false });
    expect(findFileTab(third.layout, "plan.md")?.line).toBe(40);
  });

  it("use the tab a caller minted, so its id is known before the commit", () => {
    const minted = fileTab({ path: "x.csv" });
    const { tabId } = openFileTab(layout([term("t1")]), {
      path: "x.csv",
      beside: 0,
      focus: true,
      newTab: minted,
    });
    expect(tabId).toBe(minted.id);
  });

  it("land in the left dock of an empty layout rather than leaving it empty", () => {
    const empty = layout([]);
    const placed = placeTab(empty, fileTab({ path: "a.md" }), 1, false);
    expect(placed.docks[0].tabs).toHaveLength(1);
    expect(placed.docks[1].tabs).toHaveLength(0);
  });

  it("stand beside whatever is focused when no terminal asked", () => {
    expect(besideDock(layout([term("a")], [term("b")], 1))).toBe(0);
    expect(besideDock(layout([term("a")]), "gone")).toBe(1);
  });
});

describe("browser tabs opened from outside the page", () => {
  it("reuse the tab already at the URL, without moving the keyboard", () => {
    const first = openBrowserTab(layout([term("t1")]), {
      url: "http://localhost:3000/a",
      beside: 1,
      focus: false,
    });
    // Another tab takes the dock's active slot, so reuse has something to undo.
    const other = placeTab(first.layout, browserTab({ url: "http://localhost:3000/b" }), 1, false);
    // A spelling `normalizeBrowserUrl` reduces to the same URL is the same page.
    const again = openBrowserTab(other, { url: " HTTP://LocalHost:3000/a", beside: 1, focus: false });
    expect(again.tabId).toBe(first.tabId);
    expect(again.layout.docks[1].tabs).toHaveLength(2);
    expect(again.layout.docks[1].activeId).toBe(first.tabId);
    expect(again.layout.focused).toBe(0);
    expect(findBrowserTab(again.layout, "http://localhost:3000/a")?.id).toBe(first.tabId);
  });

  it("open a new tab beside the terminal for a URL no tab is at", () => {
    const minted = browserTab({ url: "http://localhost:3000/" });
    const { layout: next, tabId } = openBrowserTab(layout([term("t1")]), {
      url: "http://localhost:3000/",
      beside: 1,
      focus: false,
      newTab: minted,
    });
    expect(tabId).toBe(minted.id);
    expect(next.docks[1].tabs.map((t) => t.id)).toEqual([minted.id]);
    expect(next.focused).toBe(0);
    expect(findBrowserTab(next, "http://localhost:3000/other")).toBeNull();
  });
});
