import { describe, expect, it } from "vitest";

import {
  isProjectNews,
  dropTargetIndex,
  otherProjectWorktreeIds,
  projectForShortcut,
  projectHolder,
  projectInitials,
  projectShortcutDigit,
  projectWorktreeIds,
  reorderedRoots,
  toggleTarget,
} from "./projects";

const wt = (id: number, repo_root: string, trashed_at = "") => ({
  id,
  repo_root,
  trashed_at,
});

const repo = (root: string, worktrees: ReturnType<typeof wt>[]) => ({
  root,
  worktrees,
});

const A = "/src/alpha";
const B = "/src/beta";
const C = "/src/gamma";

describe("which of a project's worktrees count", () => {
  it("takes the ones that are on disk to stay", () => {
    expect([...projectWorktreeIds(repo(A, [wt(1, A), wt(2, A)]))]).toEqual([1, 2]);
  });

  /** A badge lit by a checkout that is being deleted offers a click that lands
   *  nowhere — the rail already refuses to open one. */
  it("leaves out a worktree in the trash", () => {
    const r = repo(A, [wt(1, A, "2026-08-10T00:00:00Z"), wt(2, A)]);
    expect([...projectWorktreeIds(r)]).toEqual([2]);
  });

  it("answers for no project at all", () => {
    expect(projectWorktreeIds(null).size).toBe(0);
  });
});

describe("which worktrees are somewhere other than the selected project", () => {
  const repos = [repo(A, [wt(1, A)]), repo(B, [wt(2, B), wt(3, B)]), repo(C, [wt(4, C)])];

  it("skips the project on screen", () => {
    expect([...otherProjectWorktreeIds(repos, A)]).toEqual([2, 3, 4]);
    expect([...otherProjectWorktreeIds(repos, B)]).toEqual([1, 4]);
  });

  /** The user is looking at a fallback selection (a stale `?repo=`, a project
   *  removed between polls), so every project genuinely is elsewhere. */
  it("counts every project when the active root matches none", () => {
    expect([...otherProjectWorktreeIds(repos, "/gone")]).toEqual([1, 2, 3, 4]);
    expect([...otherProjectWorktreeIds(repos, null)]).toEqual([1, 2, 3, 4]);
  });

  it("leaves out trashed worktrees here too", () => {
    const rows = [repo(A, [wt(1, A)]), repo(B, [wt(2, B, "2026-08-10T00:00:00Z"), wt(3, B)])];
    expect([...otherProjectWorktreeIds(rows, A)]).toEqual([3]);
  });
});

describe("what earns a dot on the closed selector", () => {
  it("counts news", () => {
    expect(isProjectNews("attention")).toBe(true);
    expect(isProjectNews("failed")).toBe(true);
    expect(isProjectNews("finished")).toBe(true);
  });

  /**
   * The one state that is not news. A top-bar dot lit for "a build is running in
   * another project" would be lit approximately always, which costs the dot its
   * whole meaning — the menu's own rows still render `working`.
   */
  it("does not count something merely running", () => {
    expect(isProjectNews("working")).toBe(false);
    expect(isProjectNews(null)).toBe(false);
  });
});

describe("where a project already is", () => {
  const holderA = { kind: "electron", label: "Veld Desktop" };
  const holderB = { kind: "browser", label: "Chrome" };

  it("is nowhere when no worktree of it is claimed", () => {
    const elsewhere = new Map([[99, holderA]]);
    expect(projectHolder(repo(A, [wt(1, A), wt(2, A)]), elsewhere)).toBeUndefined();
  });

  it("names the client holding one of its worktrees", () => {
    const elsewhere = new Map([[2, holderA]]);
    expect(projectHolder(repo(A, [wt(1, A), wt(2, A)]), elsewhere)).toBe(holderA);
  });

  /**
   * Iterating the claims map instead would let the note flip between two equally
   * true holders on a broadcast that changed nothing, because the map's order is
   * whatever the daemon last sent.
   */
  it("picks by the project's own worktree order, not the claims map's", () => {
    const r = repo(A, [wt(1, A), wt(2, A)]);
    const claimsInOneOrder = new Map([
      [2, holderB],
      [1, holderA],
    ]);
    const claimsInTheOther = new Map([
      [1, holderA],
      [2, holderB],
    ]);
    expect(projectHolder(r, claimsInOneOrder)).toBe(holderA);
    expect(projectHolder(r, claimsInTheOther)).toBe(holderA);
  });

  /** A trashed worktree cannot be opened, so a claim on one says nothing about
   *  where the project is. */
  it("ignores a claim on a worktree in the trash", () => {
    const r = repo(A, [wt(1, A, "2026-08-10T00:00:00Z"), wt(2, A)]);
    expect(projectHolder(r, new Map([[1, holderA]]))).toBeUndefined();
    expect(projectHolder(r, new Map([[2, holderB]]))).toBe(holderB);
  });

  it("answers for no project at all", () => {
    expect(projectHolder(null, new Map([[1, holderA]]))).toBeUndefined();
  });
});

describe("the project a number key addresses", () => {
  const repos = [{ root: "/a" }, { root: "/b" }, { root: "/c" }];

  it("is the one at that position, 1-based", () => {
    expect(projectForShortcut(repos, 1)?.root).toBe("/a");
    expect(projectForShortcut(repos, 3)?.root).toBe("/c");
  });

  it("is nothing past the end of the list", () => {
    expect(projectForShortcut(repos, 4)).toBe(null);
  });

  /** ⌘0 is "reset zoom" almost everywhere, so nine is the whole keyboard's worth. */
  it("stops at nine, and refuses zero and rubbish", () => {
    const many = Array.from({ length: 12 }, (_, i) => ({ root: `/p${i}` }));
    expect(projectForShortcut(many, 9)?.root).toBe("/p8");
    expect(projectForShortcut(many, 10)).toBe(null);
    expect(projectForShortcut(many, 0)).toBe(null);
    expect(projectForShortcut(many, -1)).toBe(null);
    expect(projectForShortcut(many, 1.5)).toBe(null);
  });
});

describe("where ⌘` goes", () => {
  const repos = [{ root: "/a" }, { root: "/b" }, { root: "/c" }];

  it("goes back to the project you came from", () => {
    expect(toggleTarget(repos, "/c", "/a")?.root).toBe("/a");
  });

  /** With two projects — the case the chord exists for — the fallback is the toggle. */
  it("goes to the next one when there is no history yet", () => {
    const two = [{ root: "/a" }, { root: "/b" }];
    expect(toggleTarget(two, "/a", null)?.root).toBe("/b");
    expect(toggleTarget(two, "/b", null)?.root).toBe("/a");
  });

  /** A project removed since is not somewhere to go; fall through to the cycle
   *  rather than doing nothing. */
  it("ignores a remembered project that is gone", () => {
    expect(toggleTarget(repos, "/a", "/removed")?.root).toBe("/b");
  });

  /** After an import leaves one project standing, `previous` can decay to the
   *  current one — which is not a destination. */
  it("ignores a remembered project that is the current one", () => {
    expect(toggleTarget(repos, "/b", "/b")?.root).toBe("/c");
  });

  it("has nowhere to go with fewer than two projects", () => {
    expect(toggleTarget([{ root: "/a" }], "/a", null)).toBe(null);
    expect(toggleTarget([], null, null)).toBe(null);
  });

  it("wraps, and starts at the top when the selection matches nothing", () => {
    expect(toggleTarget(repos, "/c", null)?.root).toBe("/a");
    expect(toggleTarget(repos, "/gone", null)?.root).toBe("/a");
  });
});

describe("a project's initials", () => {
  it("takes one letter per word, up to two", () => {
    expect(projectInitials("my-api")).toBe("MA");
    expect(projectInitials("Prosperity Solutions")).toBe("PS");
    expect(projectInitials("a.b.c")).toBe("AB");
    expect(projectInitials("some/path")).toBe("SP");
  });

  it("takes two letters from a single word", () => {
    expect(projectInitials("veld")).toBe("VE");
    expect(projectInitials("x")).toBe("X");
  });

  /**
   * `slice(0, 2)` splits a surrogate pair and renders the replacement character.
   * A project named with a leading emoji is not exotic — it is what somebody does
   * to make a repo stand out.
   */
  it("does not cut an astral character in half", () => {
    expect(projectInitials("🚀rocket")).toBe("🚀R");
    expect(projectInitials("🚀 rocket")).toBe("🚀R");
    expect(projectInitials("🚀")).toBe("🚀");
  });

  it("trims before it splits", () => {
    expect(projectInitials("  spaced  ")).toBe("SP");
  });

  /**
   * A name that is nothing but separators has no word to take from, so it falls
   * back to the raw characters rather than returning nothing. Pinned as the real
   * answer because the obvious alternative — an empty square — is indistinguishable
   * from a rendering bug.
   */
  it("falls back to the raw characters when there is no word in the name", () => {
    expect(projectInitials("---")).toBe("--");
    expect(projectInitials("-")).toBe("-");
  });

  /** The one input that genuinely has nothing to show. The tooltip still names the
   *  project, and a repo with no name at all is a data anomaly rather than a case
   *  to invent a glyph for. */
  it("is empty only for an empty name", () => {
    expect(projectInitials("")).toBe("");
    expect(projectInitials("   ")).toBe("");
  });
});

describe("dragging a project to a new place", () => {
  const roots = ["/a", "/b", "/c", "/d"];

  it("moves it down", () => {
    expect(reorderedRoots(roots, 0, 2)).toEqual(["/b", "/c", "/a", "/d"]);
  });

  it("moves it up", () => {
    expect(reorderedRoots(roots, 3, 0)).toEqual(["/d", "/a", "/b", "/c"]);
  });

  it("is a no-op on itself", () => {
    expect(reorderedRoots(roots, 1, 1)).toEqual(roots);
  });

  /** A drop outside the column is not a reorder. */
  it("is a no-op for an index that is not there", () => {
    expect(reorderedRoots(roots, -1, 2)).toEqual(roots);
    expect(reorderedRoots(roots, 0, 9)).toEqual(roots);
    expect(reorderedRoots(roots, 9, 0)).toEqual(roots);
  });

  it("never returns the array it was given", () => {
    const out = reorderedRoots(roots, 1, 1);
    expect(out).not.toBe(roots);
  });
});

describe("turning a caret position into a destination index", () => {
  /** Dragging down: every caret past the item's own slot shifts by one, because
   *  the item is removed before it is re-inserted. */
  it("shifts down for a move towards the end", () => {
    expect(dropTargetIndex(0, 2)).toBe(1);
    expect(dropTargetIndex(0, 4)).toBe(3);
    expect(dropTargetIndex(1, 3)).toBe(2);
  });

  /** Dragging up: the caret index is already the destination. */
  it("is the caret itself for a move towards the start", () => {
    expect(dropTargetIndex(3, 0)).toBe(0);
    expect(dropTargetIndex(3, 1)).toBe(1);
    expect(dropTargetIndex(2, 1)).toBe(1);
  });

  /**
   * **Both carets touching an item mean "stay".** The one above it and the one
   * below it are the same position once the item is lifted out, and writing the
   * whole order back to the daemon to change nothing is the bug this catches.
   */
  it("is nothing when the item would not move", () => {
    expect(dropTargetIndex(2, 2)).toBe(null);
    expect(dropTargetIndex(2, 3)).toBe(null);
    expect(dropTargetIndex(0, 0)).toBe(null);
    expect(dropTargetIndex(0, 1)).toBe(null);
  });

  /** The caret past the last item — "drop at the very end". */
  it("handles the trailing caret", () => {
    expect(dropTargetIndex(0, 3)).toBe(2);
    expect(dropTargetIndex(2, 3)).toBe(null);
  });
});

describe("which digit a keystroke means", () => {
  /** `code` names the physical key, which is what ⌘2 means to a person — on AZERTY
   *  the unshifted digit row is punctuation. */
  it("prefers the physical key over the character it prints", () => {
    expect(projectShortcutDigit("Digit2", "é")).toBe("2");
    expect(projectShortcutDigit("Digit9", "ç")).toBe("9");
  });

  it("falls back to the character when there is no code", () => {
    expect(projectShortcutDigit("", "3")).toBe("3");
  });

  /** The bound lives here and nowhere else — this is what makes
   *  MAX_PROJECT_SHORTCUTS authoritative rather than decorative. */
  it("refuses a digit outside the addressable range", () => {
    expect(projectShortcutDigit("Digit0", "0")).toBe(null);
    expect(projectShortcutDigit("", "0")).toBe(null);
  });

  it("refuses anything that is not a digit", () => {
    expect(projectShortcutDigit("KeyB", "b")).toBe(null);
    expect(projectShortcutDigit("Backquote", "`")).toBe(null);
    expect(projectShortcutDigit("", "")).toBe(null);
  });
});

describe("initials that case-expand", () => {
  /**
   * `ß`.toUpperCase() is `SS` and the `ﬁ` ligature is `FI`, so slicing first and
   * upper-casing after produced three glyphs in a 26px square. Cutting after the
   * expansion is what keeps "up to two characters" true.
   */
  it("still yields at most two characters", () => {
    expect(projectInitials("ßeta")).toBe("SS");
    expect(projectInitials("ﬁnance")).toBe("FI");
    expect([...projectInitials("ßeta gamma")].length).toBe(2);
  });
});
