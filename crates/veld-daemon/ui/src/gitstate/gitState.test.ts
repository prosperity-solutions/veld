import { describe, expect, it } from "vitest";

import type { WorktreeGitSignals } from "../api";
import { gitDescription, gitTooltipLines, rowGitState } from "./gitState";

function signals(over: Partial<WorktreeGitSignals> = {}): WorktreeGitSignals {
  return {
    dirty: false,
    upstream: "origin/feat-x",
    ahead: 0,
    behind: 0,
    upstream_gone: false,
    ...over,
  };
}

describe("rowGitState", () => {
  it("shows nothing for a worktree the daemon knows nothing about", () => {
    // The first poll of a freshly opened window, and every worktree whose git
    // probes failed. Must not read as "clean".
    expect(rowGitState(undefined)).toBeNull();
  });

  it("shows synced for a clean, fully pushed branch", () => {
    // The quiet resting state, and the first step of the progression a reader
    // follows. It asserts nothing about a pull request — core never asked a forge.
    expect(rowGitState(signals())).toBe("synced");
  });

  /**
   * **`dirty === false`, never merely falsy.** `null` is "the sweep has not looked
   * yet", and a row must not claim everything is pushed on the strength of a
   * reading that never happened — which is the whole reason the two are separate
   * values on the wire.
   */
  it("will not claim synced before the dirty sweep has answered", () => {
    expect(rowGitState(signals({ dirty: null }))).toBeNull();
  });

  it("shows nothing at all for a branch that was never pushed", () => {
    // No upstream means nothing to be in sync *with*. This is also what keeps a
    // never-pushed branch from ever being confused with a merged-and-deleted one.
    expect(
      rowGitState(signals({ upstream: null, ahead: null, behind: null })),
    ).toBeNull();
  });

  it("shows dirty for uncommitted work", () => {
    expect(rowGitState(signals({ dirty: true }))).toBe("dirty");
  });

  it("shows unpushed for commits the upstream does not have", () => {
    expect(rowGitState(signals({ ahead: 2 }))).toBe("unpushed");
  });

  it("shows gone for a deleted upstream branch", () => {
    // git reports no counts for `[gone]`, which is why `ahead` is null here.
    expect(
      rowGitState(signals({ ahead: null, behind: null, upstream_gone: true })),
    ).toBe("gone");
  });

  /**
   * The one ordering that carries a consequence: every other state describes work
   * that is safely somewhere else, and dirty describes work that is not. A merged
   * worktree with a stray edit must not read as safe to throw away.
   */
  it("ranks dirty above a gone upstream", () => {
    expect(
      rowGitState(
        signals({ dirty: true, ahead: null, behind: null, upstream_gone: true }),
      ),
    ).toBe("dirty");
  });

  it("ranks dirty above unpushed", () => {
    expect(rowGitState(signals({ dirty: true, ahead: 3 }))).toBe("dirty");
  });

  it("treats an unknown dirty as not dirty rather than as a state", () => {
    // `dirty: null` is "the sweep has not reached this worktree". The push half
    // is known independently and still gets to speak.
    expect(rowGitState(signals({ dirty: null, ahead: 1 }))).toBe("unpushed");
    expect(rowGitState(signals({ dirty: null }))).toBeNull();
  });

  it("does not treat being behind as a state of its own", () => {
    // On the wire, unrendered — the top bar's staleness pill already answers it.
    // Such a row is still `synced`: nothing of *yours* is unpushed.
    expect(rowGitState(signals({ behind: 4 }))).toBe("synced");
  });

  it("ranks synced last — it is the absence of anything to do", () => {
    expect(rowGitState(signals({ dirty: true }))).toBe("dirty");
    expect(rowGitState(signals({ ahead: 1 }))).toBe("unpushed");
    expect(
      rowGitState(signals({ ahead: null, behind: null, upstream_gone: true })),
    ).toBe("gone");
  });
});

describe("gitTooltipLines", () => {
  it("is empty when the daemon has said nothing", () => {
    expect(gitTooltipLines(undefined)).toEqual([]);
    expect(gitTooltipLines(signals({ dirty: null, upstream: null }))).toEqual([]);
  });

  it("says what was measured for a synced branch, and nothing more", () => {
    const lines = gitTooltipLines(signals());
    expect(lines).toEqual(["Everything is pushed to origin/feat-x"]);
    // Never "and no pull request exists" — veld did not ask a forge.
    expect(lines.join("")).not.toMatch(/pull request|PR/i);
  });

  /**
   * The whole reason the wire carries facts rather than a state: the glyph shows
   * the winner, the tooltip shows all of them.
   */
  it("lists every fact, not just the one the glyph shows", () => {
    expect(gitTooltipLines(signals({ dirty: true, ahead: 3, behind: 2 }))).toEqual([
      "Uncommitted changes",
      "3 commits not pushed to origin/feat-x",
      "2 commits behind origin/feat-x",
    ]);
  });

  it("pluralises a single commit", () => {
    expect(gitTooltipLines(signals({ ahead: 1 }))).toEqual([
      "1 commit not pushed to origin/feat-x",
    ]);
  });

  /** Core measured a deleted ref; it does not claim to know a PR was merged. */
  it("glosses a gone upstream without asserting the merge", () => {
    const [line] = gitTooltipLines(
      signals({ ahead: null, behind: null, upstream_gone: true }),
    );
    expect(line).toContain("origin/feat-x is gone");
    expect(line).toContain("usually means its pull request was merged");
  });

  it("says a branch has no upstream only alongside another fact", () => {
    const none = { upstream: null, ahead: null, behind: null };
    expect(gitTooltipLines(signals({ ...none, dirty: true }))).toEqual([
      "Uncommitted changes",
      "This branch has no upstream — nothing has been pushed",
    ]);
    // Nothing else to say, and no glyph to hang it on.
    expect(gitTooltipLines(signals({ ...none, dirty: null }))).toEqual([]);
  });

  it("never leaves an upstream name as the literal null", () => {
    expect(
      gitTooltipLines(signals({ upstream: null, upstream_gone: true })).join(""),
    ).toContain("its upstream is gone");
  });
});

describe("gitDescription", () => {
  it("is undefined when no glyph renders, so the row adds no clause", () => {
    expect(gitDescription(undefined)).toBeUndefined();
    expect(gitDescription(signals({ dirty: null }))).toBeUndefined();
    expect(gitDescription(signals({ upstream: null, ahead: null }))).toBeUndefined();
  });

  it("describes each state in words for a screen reader", () => {
    expect(gitDescription(signals({ dirty: true }))).toBe("uncommitted changes");
    expect(gitDescription(signals({ ahead: 2 }))).toBe("2 commits not pushed");
    expect(gitDescription(signals({ ahead: 1 }))).toBe("1 commit not pushed");
    expect(
      gitDescription(signals({ ahead: null, behind: null, upstream_gone: true })),
    ).toBe("upstream branch deleted");
    expect(gitDescription(signals())).toBe("everything pushed");
  });
});
