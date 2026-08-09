import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { LayoutSaveResult, PaneLayoutDoc } from "../api";
import type { PaneLayout } from "../panes/model";

/**
 * The daemon, replaced by something a test can drive.
 *
 * Only the two layout calls; `ideTicket` is here because `channel.ts` is pulled
 * in transitively and must not try to open a socket from a test.
 */
const reads = new Map<number, PaneLayoutDoc>();
const writes: { worktreeId: number; version: number; layout: unknown | null }[] = [];
let nextWrite: (worktreeId: number) => LayoutSaveResult = (worktreeId) => ({
  ok: true,
  doc: { version: (reads.get(worktreeId)?.version ?? 0) + 1, layout: {} },
});

vi.mock("../api", () => ({
  api: {
    paneLayout: (worktreeId: number) =>
      Promise.resolve(reads.get(worktreeId) ?? { version: 0, layout: null }),
    putPaneLayout: (worktreeId: number, version: number, layout: unknown | null) => {
      writes.push({ worktreeId, version, layout });
      return Promise.resolve(nextWrite(worktreeId));
    },
    ideTicket: () => Promise.resolve({ ticket: "t", expires_in_ms: 1000 }),
  },
}));

const {
  dropLayout,
  onExternalLayoutChange,
  readLayout,
  syncLayouts,
  writeLayout,
  forgetVersions,
} = await import("./layoutStore");

/** A minimal layout with one terminal, which is all any of this cares about. */
function layoutWith(id: string, ratio = 0.5): PaneLayout {
  return {
    docks: [
      { tabs: [{ id, kind: "terminal", title: "Terminal" }], activeId: id },
      { tabs: [], activeId: null },
    ],
    ratio,
    focused: 0,
  };
}

const emptyLayout: PaneLayout = {
  docks: [
    { tabs: [], activeId: null },
    { tabs: [], activeId: null },
  ],
  ratio: 0.5,
  focused: 0,
};

/** A `localStorage` a test can inspect, since the suite runs without a DOM. */
function installStorage(initial: Record<string, string> = {}) {
  const map = new Map(Object.entries(initial));
  const fake = {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, v),
    removeItem: (k: string) => void map.delete(k),
  };
  vi.stubGlobal("localStorage", fake);
  return map;
}

const LEGACY = "veld.panes.worktrees.v1";

beforeEach(() => {
  vi.useFakeTimers();
  reads.clear();
  writes.length = 0;
  forgetVersions();
  onExternalLayoutChange(() => {});
  nextWrite = (worktreeId) => ({
    ok: true,
    doc: { version: (reads.get(worktreeId)?.version ?? 0) + 1, layout: {} },
  });
  installStorage();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/** Run the debounce out and let the write's promise settle. */
async function settle() {
  await vi.runAllTimersAsync();
}

describe("reading", () => {
  it("returns null for a worktree nobody has arranged", async () => {
    expect(await readLayout(7)).toBeNull();
  });

  it("parses what the daemon holds", async () => {
    reads.set(7, { version: 3, layout: layoutWith("a") });
    const got = await readLayout(7);
    expect(got?.docks[0].tabs[0].id).toBe("a");
  });

  /**
   * A layout written by a newer build, or hand-edited in `sqlite3`. It must read
   * as "no saved layout" rather than throw — this runs on the path that renders
   * the app.
   */
  it("degrades to no layout on a document it cannot parse", async () => {
    reads.set(7, { version: 3, layout: { nonsense: true } });
    expect(await readLayout(7)).toBeNull();
  });

  /**
   * …and it must still remember the version it saw. Recording 0 here would make
   * the next save present 0, lose the check against the unreadable row, and
   * adopt exactly the document it just rejected.
   */
  it("records the version of a document it could not parse", async () => {
    reads.set(7, { version: 3, layout: { nonsense: true } });
    await readLayout(7);
    writeLayout(7, layoutWith("a"));
    await settle();
    expect(writes[0].version).toBe(3);
  });
});

describe("adopting the layout the browser store used to hold", () => {
  it("takes a worktree's panes over when the daemon has none", async () => {
    installStorage({ [LEGACY]: JSON.stringify({ 7: layoutWith("old-shell") }) });
    const got = await readLayout(7);
    expect(got?.docks[0].tabs[0].id).toBe("old-shell");
    // …and writes it back, so the next client sees it too. `expected: 0` is what
    // makes this safe against a second client doing the same thing.
    await settle();
    expect(writes).toEqual([
      { worktreeId: 7, version: 0, layout: expect.objectContaining({ ratio: 0.5 }) },
    ]);
  });

  it("removes the entry once adopted, so it is a one-time event", async () => {
    const map = installStorage({
      [LEGACY]: JSON.stringify({ 7: layoutWith("a"), 9: layoutWith("b") }),
    });
    await readLayout(7);
    expect(Object.keys(JSON.parse(map.get(LEGACY) as string))).toEqual(["9"]);
  });

  it("removes an entry it could not parse rather than retrying it forever", async () => {
    const map = installStorage({ [LEGACY]: JSON.stringify({ 7: { junk: true } }) });
    expect(await readLayout(7)).toBeNull();
    expect(JSON.parse(map.get(LEGACY) as string)).toEqual({});
  });

  /** The database wins outright: it is the newer answer by construction. */
  it("ignores the old store when the daemon has a layout", async () => {
    installStorage({ [LEGACY]: JSON.stringify({ 7: layoutWith("stale") }) });
    reads.set(7, { version: 2, layout: layoutWith("current") });
    const got = await readLayout(7);
    expect(got?.docks[0].tabs[0].id).toBe("current");
  });

  it("survives storage being unusable", async () => {
    vi.stubGlobal("localStorage", {
      getItem() {
        throw new Error("denied");
      },
      setItem() {
        throw new Error("denied");
      },
    });
    expect(await readLayout(7)).toBeNull();
  });
});

describe("writing", () => {
  it("collapses a gesture into one write", async () => {
    writeLayout(7, layoutWith("a", 0.2));
    writeLayout(7, layoutWith("a", 0.3));
    writeLayout(7, layoutWith("a", 0.4));
    await settle();
    expect(writes).toHaveLength(1);
    expect((writes[0].layout as PaneLayout).ratio).toBe(0.4);
  });

  /**
   * The app's save effect fires on every `layouts` change, including ones that
   * only touched a different worktree. Without this every drag in worktree 7
   * would also rewrite every other worktree the client has visited.
   */
  it("does not re-send an unchanged layout", async () => {
    writeLayout(7, layoutWith("a"));
    await settle();
    expect(writes).toHaveLength(1);
    writeLayout(7, layoutWith("a"));
    await settle();
    expect(writes).toHaveLength(1);
  });

  it("advances the version so the next write is not stale", async () => {
    writeLayout(7, layoutWith("a", 0.2));
    await settle();
    writeLayout(7, layoutWith("a", 0.3));
    await settle();
    expect(writes.map((w) => w.version)).toEqual([0, 1]);
  });

  /**
   * A worktree whose last pane was closed must have no row, so the next client
   * to open it seeds a default rather than restoring an empty screen.
   */
  it("deletes rather than storing a layout with no panes", async () => {
    writeLayout(7, emptyLayout);
    await settle();
    expect(writes[0].layout).toBeNull();
  });

  /**
   * **Omission is not deletion.** A client that yields a worktree drops it from
   * its own state while its panes go on existing for whoever takes it.
   */
  it("writes only the worktrees it is given", async () => {
    syncLayouts({ 7: layoutWith("a"), 9: layoutWith("b") });
    await settle();
    expect(writes.map((w) => w.worktreeId).sort()).toEqual([7, 9]);
    writes.length = 0;
    syncLayouts({ 7: layoutWith("a", 0.9) });
    await settle();
    expect(writes.map((w) => w.worktreeId)).toEqual([7]);
  });

  it("keeps the version where it was when a write fails", async () => {
    nextWrite = () => {
      throw new Error("daemon down");
    };
    writeLayout(7, layoutWith("a"));
    await settle();
    nextWrite = () => ({ ok: true, doc: { version: 1, layout: {} } });
    writeLayout(7, layoutWith("a", 0.9));
    await settle();
    // Still 0: a failed write must not advance a version, or the next one would
    // claim to have read a row it never saw.
    expect(writes.map((w) => w.version)).toEqual([0, 0]);
  });
});

describe("losing the version check", () => {
  /**
   * The hand-off. The client that just let a worktree go can still have a
   * debounced save in flight; it must adopt the winner's panes rather than
   * retry, because a retry restores a layout naming terminal sessions the new
   * owner is attached to.
   */
  it("adopts the winner's layout instead of retrying", async () => {
    const adopted: [number, PaneLayout | null][] = [];
    onExternalLayoutChange((id, l) => adopted.push([id, l]));
    nextWrite = () => ({ ok: false, conflict: { version: 5, layout: layoutWith("theirs") } });

    writeLayout(7, layoutWith("mine"));
    await settle();

    expect(writes).toHaveLength(1);
    expect(adopted).toHaveLength(1);
    expect(adopted[0][0]).toBe(7);
    expect(adopted[0][1]?.docks[0].tabs[0].id).toBe("theirs");
  });

  it("takes the winner's version, so the next write is against the truth", async () => {
    nextWrite = () => ({ ok: false, conflict: { version: 5, layout: layoutWith("theirs") } });
    writeLayout(7, layoutWith("mine"));
    await settle();
    nextWrite = () => ({ ok: true, doc: { version: 6, layout: {} } });
    writeLayout(7, layoutWith("next"));
    await settle();
    expect(writes[1].version).toBe(5);
  });

  it("drops the worktree when the winner deleted it", async () => {
    const adopted: [number, PaneLayout | null][] = [];
    onExternalLayoutChange((id, l) => adopted.push([id, l]));
    nextWrite = () => ({ ok: false, conflict: { version: 0, layout: null } });
    writeLayout(7, layoutWith("mine"));
    await settle();
    expect(adopted).toEqual([[7, null]]);
  });
});

describe("dropping a worktree", () => {
  it("cancels a queued write, so a deleted worktree is not recreated", async () => {
    writeLayout(7, layoutWith("a"));
    dropLayout(7);
    await settle();
    // One call, and it is the delete — not the queued layout.
    expect(writes).toEqual([{ worktreeId: 7, version: 0, layout: null }]);
  });
});
