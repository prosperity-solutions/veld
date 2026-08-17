import { describe, expect, it } from "vitest";

import type { KeyValueStore } from "./lastWorktree";
import {
  lastWorktreeName,
  recallLastWorktree,
  rememberLastWorktree,
  worktreeKeyToReopen,
} from "./lastWorktree";

/** `usePersistedPerWindow`'s key pair, spelled out here rather than imported:
 *  `selectionKeys` lives in `App.tsx` and this module is deliberately given the
 *  pair rather than deriving it, so the tests state the shape they assume. */
const keys = (name: string, slot?: string): [string, string] =>
  slot ? [`${name}.slot.${slot}`, name] : [name, name];

const fake = (seed: Record<string, string> = {}): KeyValueStore & {
  map: Map<string, string>;
} => {
  const map = new Map(Object.entries(seed));
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => {
      map.set(k, v);
    },
  };
};

const wt = (id: number, path: string, over: { trashed_at?: string } = {}) => ({
  id,
  path,
  trashed_at: "",
  ...over,
});

describe("the key a project's memory lives under", () => {
  it("is one name per project root", () => {
    expect(lastWorktreeName("/a")).not.toBe(lastWorktreeName("/b"));
  });

  /**
   * The key is *not* injective once `selectionKeys` appends `.slot.<slot>`, and
   * that is stated here rather than assumed away: a project rooted at a
   * directory named `…​.slot.main` collides with the `main` slot's entry for the
   * shorter root. What makes it harmless is the resolve step, asserted below —
   * so this test pins the collision *and* the thing that absorbs it.
   */
  it("can collide with a slot suffix, and the resolve absorbs it", () => {
    const store = fake();
    rememberLastWorktree(store, keys(lastWorktreeName("/repo"), "main"), wt(1, "/wt/a"));
    expect(recallLastWorktree(store, keys(lastWorktreeName("/repo.slot.main")))).toBe("/wt/a");
    // …and the other project's worktrees do not contain that path, so the read
    // is "no opinion" rather than somebody else's checkout.
    expect(worktreeKeyToReopen([wt(1, "/other/wt")], "/wt/a")).toBe("");
  });
});

describe("recording where a project is", () => {
  it("writes the scoped key and the unscoped one together", () => {
    const store = fake();
    rememberLastWorktree(store, keys("veld.lastWorktree./repo", "main"), wt(1, "/wt/a"));
    expect(store.map.get("veld.lastWorktree./repo.slot.main")).toBe("/wt/a");
    expect(store.map.get("veld.lastWorktree./repo")).toBe("/wt/a");
  });

  /** A window slot reads its own key first, so two windows on one project sit on
   *  their own worktrees — the reason layouts are per slot in the first place. */
  it("lets each window slot hold its own answer", () => {
    const store = fake();
    rememberLastWorktree(store, keys("n", "w1"), wt(1, "/wt/a"));
    rememberLastWorktree(store, keys("n", "w2"), wt(2, "/wt/b"));
    expect(recallLastWorktree(store, keys("n", "w1"))).toBe("/wt/a");
    expect(recallLastWorktree(store, keys("n", "w2"))).toBe("/wt/b");
  });

  /** …and a brand-new slot inherits the last thing written anywhere, which is
   *  what makes the first switch in a fresh window land somewhere sensible. */
  it("seeds a slot that has nothing of its own from the unscoped key", () => {
    const store = fake();
    rememberLastWorktree(store, keys("n", "w1"), wt(1, "/wt/a"));
    expect(recallLastWorktree(store, keys("n", "w9"))).toBe("/wt/a");
  });

  it("says nothing for a project it has never recorded", () => {
    expect(recallLastWorktree(fake(), keys("n", "w1"))).toBe("");
  });

  /** `""` is "no opinion", so storing it would be storing the absence of an
   *  answer over a real one — a transient empty selection must not erase where
   *  the project actually was. */
  it("never records an empty path over a real one", () => {
    const store = fake();
    rememberLastWorktree(store, keys("n", "w1"), wt(1, "/wt/a"));
    rememberLastWorktree(store, keys("n", "w1"), wt(1, ""));
    expect(recallLastWorktree(store, keys("n", "w1"))).toBe("/wt/a");
  });

  /**
   * The invariant the signature exists to enforce, asserted from the other side.
   *
   * `rememberLastWorktree` takes the row rather than a string precisely so
   * `String(worktree.id)` — the natural wrong thing, and the same type as a path —
   * cannot be passed. This pins what the failure would have looked like if it
   * could: not an error, but every project silently falling back to `main`.
   */
  it("would resolve to nothing if an id were ever stored instead of a path", () => {
    expect(worktreeKeyToReopen([wt(7, "/wt/a")], String(7))).toBe("");
  });

  /** Private browsing and a full quota both throw from `setItem`. The cost is
   *  landing on the main checkout next time — the behaviour this replaces — so
   *  it must not be an unhandled error in a click handler. */
  it("survives a storage that throws", () => {
    const throwing: KeyValueStore = {
      getItem: () => {
        throw new Error("nope");
      },
      setItem: () => {
        throw new Error("nope");
      },
    };
    expect(() => rememberLastWorktree(throwing, keys("n"), wt(1, "/wt/a"))).not.toThrow();
    expect(recallLastWorktree(throwing, keys("n"))).toBe("");
  });
});

describe("resolving a remembered project back to a selection", () => {
  const rows = [wt(1, "/wt/main"), wt(2, "/wt/feature")];

  it("names the remembered worktree", () => {
    expect(worktreeKeyToReopen(rows, "/wt/feature")).toBe("2");
  });

  it("has no opinion when nothing was remembered", () => {
    expect(worktreeKeyToReopen(rows, "")).toBe("");
  });

  /** The memory outlives the worktree: deleted, or moved to another machine's
   *  paths. Either has to read as *unremembered*, because the caller's fallback
   *  (main checkout, then the first row) is a correct answer. */
  it("has no opinion when the remembered worktree is gone", () => {
    expect(worktreeKeyToReopen(rows, "/wt/deleted")).toBe("");
  });

  /** Same for the trash: still on disk, and about to stop being. */
  it("has no opinion when the remembered worktree is in the trash", () => {
    const trashed = [wt(1, "/wt/main"), wt(2, "/wt/feature", { trashed_at: "2026-08-10" })];
    expect(worktreeKeyToReopen(trashed, "/wt/feature")).toBe("");
  });

  /**
   * The reason the stored value is a path and not the row id.
   *
   * Worktree rowids are reused: delete a checkout and the next one created can be
   * handed the same number. Had the id been stored, this recall would resolve to
   * a worktree the user has never opened — silently, and in the same project.
   */
  it("does not resurrect a recycled row id", () => {
    const before = [wt(7, "/wt/old")];
    const after = [wt(7, "/wt/brand-new")];
    const store = fake();
    rememberLastWorktree(store, keys("n"), before[0]);
    expect(worktreeKeyToReopen(after, recallLastWorktree(store, keys("n")))).toBe("");
  });
});
