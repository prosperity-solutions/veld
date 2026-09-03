import { describe, expect, it } from "vitest";

import type { KeyValueStore } from "./foldedSections";
import {
  defaultFoldedSections,
  foldedSectionsKey,
  forgetFoldedSection,
  readFoldedSections,
  renameFoldedSection,
  toggleFoldedSection,
  writeFoldedSections,
} from "./foldedSections";

const fake = (
  seed: Record<string, string> = {},
): KeyValueStore & { map: Map<string, string> } => {
  const map = new Map(Object.entries(seed));
  return {
    map,
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => {
      map.set(k, v);
    },
  };
};

/**
 * Two section keys that are not lane names, written out rather than imported:
 * apart from the one section it starts folded, this module is ignorant of what a
 * section key means, and the tests state the shapes it has to survive.
 *
 * The trash sentinel is built rather than typed as a literal so this file holds
 * no NUL byte of its own — `App.tsx` does, and it is why `grep` needs `-a`
 * there.
 */
const TRASH = `${String.fromCharCode(0)}trash`;
const UNGROUPED = "";

/**
 * A store that has already recorded "/repo" as having *nothing* folded.
 *
 * The mutator tests below are about the mutators, and a fresh store now starts
 * with the trash folded — which would otherwise appear in every one of their
 * expectations as noise unrelated to what they check. This is a real state, not a
 * test fixture: it is what the user reaches by unfolding the trash once.
 */
const opened = (): KeyValueStore & { map: Map<string, string> } => {
  const store = fake();
  writeFoldedSections(store, "/repo", new Set());
  return store;
};

describe("the key a project's folds live under", () => {
  it("is one name per project root", () => {
    expect(foldedSectionsKey("/a")).not.toBe(foldedSectionsKey("/b"));
  });
});

describe("reading what a project has folded", () => {
  /** The trash, and only the trash — every other section starts open. */
  it("gives a project it has never recorded the defaults", () => {
    expect(readFoldedSections(fake(), "/repo")).toEqual(new Set([TRASH]));
    expect(defaultFoldedSections()).toEqual(new Set([TRASH]));
  });

  /** A default is a starting point, not a floor: unfolding the trash records a
   *  set without it, and that set is what comes back. */
  it("remembers a trash the user has unfolded", () => {
    const store = fake();
    toggleFoldedSection(store, "/repo", TRASH);
    expect(readFoldedSections(store, "/repo").size).toBe(0);
  });

  /**
   * Why the default lives inside the read rather than in the rail's hook.
   *
   * `toggleFoldedSection` re-reads before it writes. A default applied by the
   * caller would be invisible to that re-read, so folding an unrelated lane would
   * write back a set with no trash in it — springing the trash open as a side
   * effect of a click that had nothing to do with it.
   */
  it("keeps the trash folded when another section is folded first", () => {
    const store = fake();
    expect(toggleFoldedSection(store, "/repo", "Archived")).toEqual(
      new Set([TRASH, "Archived"]),
    );
  });

  /** Callers mutate what they are handed, so the defaults must not be shared. */
  it("hands out a fresh set of defaults each time", () => {
    const first = readFoldedSections(fake(), "/repo");
    first.clear();
    expect(readFoldedSections(fake(), "/repo").has(TRASH)).toBe(true);
  });

  /** The sentinel keys carry a NUL, which JSON escapes on the way out and
   *  restores on the way back. Pinned, because a delimited-string encoding would
   *  have made this the interesting case rather than a non-event. */
  it("round-trips the section keys it was given", () => {
    const store = fake();
    writeFoldedSections(store, "/repo", new Set(["Archived", TRASH]));
    expect(readFoldedSections(store, "/repo")).toEqual(new Set(["Archived", TRASH]));
  });

  /** The ungrouped section's key IS the empty string, which is the other half of
   *  the reason the stored shape is a JSON array: `"".split(",")` cannot tell
   *  "nothing folded" from "the ungrouped section is folded". */
  it("treats the empty string as a real section key", () => {
    const store = fake();
    writeFoldedSections(store, "/repo", new Set([UNGROUPED]));
    expect(readFoldedSections(store, "/repo").has(UNGROUPED)).toBe(true);
  });

  it("keeps one project's folds out of another's", () => {
    const store = fake();
    writeFoldedSections(store, "/a", new Set(["Archived"]));
    // Untouched, so `/b` is a project that has said nothing — the defaults, and
    // nothing of `/a`'s.
    expect(readFoldedSections(store, "/b")).toEqual(new Set([TRASH]));
  });

  /**
   * Every unreadable value has the same correct answer: nothing is folded — and
   * pointedly NOT the defaults, which are for a project that has said nothing
   * rather than one whose answer could not be read.
   *
   * A fold that fails *open* shows more rows than expected; one that failed
   * *shut* would hide worktrees with no explanation, which is the failure this
   * feature must not have.
   */
  it.each([
    ["not JSON", "{{{"],
    ["a JSON scalar", '"Archived"'],
    ["a JSON object", '{"Archived":true}'],
    ["null", "null"],
  ])("reads %s as nothing folded", (_name, raw) => {
    const store = fake({ [foldedSectionsKey("/repo")]: raw });
    expect(readFoldedSections(store, "/repo").size).toBe(0);
  });

  /** A future version writing richer entries must not put non-keys into the set,
   *  where they would compare unequal to every real section key anyway. */
  it("drops entries that are not strings", () => {
    const store = fake({ [foldedSectionsKey("/repo")]: '["Archived",3,null,{}]' });
    expect(readFoldedSections(store, "/repo")).toEqual(new Set(["Archived"]));
  });

  it("survives a storage that throws", () => {
    const throwing: KeyValueStore = {
      getItem: () => {
        throw new Error("nope");
      },
      setItem: () => {
        throw new Error("nope");
      },
    };
    expect(readFoldedSections(throwing, "/repo").size).toBe(0);
    expect(() => writeFoldedSections(throwing, "/repo", new Set(["a"]))).not.toThrow();
  });
});

describe("folding a section shut and open again", () => {
  it("folds, then unfolds", () => {
    const store = opened();
    expect(toggleFoldedSection(store, "/repo", "Archived").has("Archived")).toBe(true);
    expect(toggleFoldedSection(store, "/repo", "Archived").has("Archived")).toBe(false);
  });

  it("leaves the other sections alone", () => {
    const store = opened();
    toggleFoldedSection(store, "/repo", "Archived");
    toggleFoldedSection(store, "/repo", TRASH);
    toggleFoldedSection(store, "/repo", "Archived");
    expect(readFoldedSections(store, "/repo")).toEqual(new Set([TRASH]));
  });

  /**
   * The reason the toggle re-reads instead of transforming a set handed to it.
   *
   * Two windows share one key. The second window's React state was taken before
   * the first window's fold landed, so a toggle built from that snapshot would
   * write it back and silently undo the other window's change.
   */
  it("does not clobber a fold another window made since the last read", () => {
    const store = opened();
    // This window's snapshot: nothing folded.
    const stale = readFoldedSections(store, "/repo");
    // ...meanwhile, the other window folds the trash.
    toggleFoldedSection(store, "/repo", TRASH);
    // This window now folds a lane, from its stale view of the world.
    expect(stale.size).toBe(0);
    expect(toggleFoldedSection(store, "/repo", "Archived")).toEqual(
      new Set([TRASH, "Archived"]),
    );
  });
});

describe("keeping folds in step with the lanes they name", () => {
  it("carries a fold through a rename", () => {
    const store = opened();
    toggleFoldedSection(store, "/repo", "Archived");
    expect(renameFoldedSection(store, "/repo", "Archived", "Old")).toEqual(
      new Set(["Old"]),
    );
  });

  it("does nothing when the renamed lane was not folded", () => {
    const store = opened();
    toggleFoldedSection(store, "/repo", "Other");
    expect(renameFoldedSection(store, "/repo", "Archived", "Old")).toEqual(
      new Set(["Other"]),
    );
  });

  /** Not reachable from the UI (the rename dialog refuses a taken name), but a
   *  set collapses it to one entry rather than growing a duplicate. */
  it("collapses a rename onto an already-folded name", () => {
    const store = fake();
    writeFoldedSections(store, "/repo", new Set(["Archived", "Old"]));
    expect(renameFoldedSection(store, "/repo", "Archived", "Old")).toEqual(
      new Set(["Old"]),
    );
  });

  /**
   * Why delete has to be explicit: a lane is identified by `(repo root, name)`
   * and has no id, so a new lane created with a deleted one's name is
   * indistinguishable from it here. Without this, it would arrive folded.
   */
  it("drops a deleted lane's fold, so the name can be reused", () => {
    const store = opened();
    toggleFoldedSection(store, "/repo", "Archived");
    forgetFoldedSection(store, "/repo", "Archived");
    expect(readFoldedSections(store, "/repo").has("Archived")).toBe(false);
  });

  /** Deleting an *open* lane is the common case and must not touch storage: a
   *  write there wakes every other window's `storage` listener for nothing. */
  it("does not write when the deleted lane was not folded", () => {
    const store = fake();
    forgetFoldedSection(store, "/repo", "Archived");
    expect(store.map.size).toBe(0);
  });
});
