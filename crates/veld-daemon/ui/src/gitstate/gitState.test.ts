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

  /**
   * **Every state is work that is not safe yet, and nothing else is a state.** A
   * branch glyph for "everything is pushed" was built and removed: it does not
   * communicate not-yet-saved work, and it was permanently lit on the main
   * checkout, which never leaves that state.
   */
  it("shows nothing for a clean, fully pushed branch", () => {
    expect(rowGitState(signals())).toBeNull();
    // Not measured and measured-clean render the same blank space, which is what
    // the rail means by absence everywhere else.
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

  /**
   * **No merged glyph, and this is the assertion that keeps it honest.** A deleted
   * upstream renders nothing rather than something confidently wrong — git cannot
   * tell a merged pull request from one closed without merging and then deleted.
   *
   * Worth an explicit assertion rather than being implied by the two-state union:
   * a deleted upstream is the state somebody will reach for a glyph for, and this
   * is the line that says the answer is no. (An earlier revision of this comment
   * described a fall-through into a `synced` state that no longer exists.)
   */
  it("shows nothing for a deleted upstream", () => {
    expect(
      rowGitState(signals({ ahead: null, behind: null, upstream_gone: true })),
    ).toBeNull();
    expect(rowGitState(signals({ upstream_gone: true }))).toBeNull();
  });

  /**
   * The one ordering that carries a consequence: every other state describes work
   * that is safely somewhere else, and dirty describes work that is not. A
   * merged-and-tidied worktree with a stray edit must not read as safe to bin.
   */
  it("ranks dirty above a deleted upstream", () => {
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
    // On the wire, unrendered: being behind is not work you are holding, and the
    // top bar's staleness pill already answers it for the main checkout.
    expect(rowGitState(signals({ behind: 4 }))).toBeNull();
  });
});

describe("gitTooltipLines", () => {
  it("is empty when the daemon has said nothing", () => {
    expect(gitTooltipLines(undefined)).toEqual([]);
    expect(gitTooltipLines(signals({ dirty: null, upstream: null }))).toEqual([]);
  });

  it("still reports a deleted upstream, which is why the field is sent", () => {
    // Reachable only when another fact holds the row's glyph slot — a dirty tree,
    // or an activity glyph. This is now `upstream_gone`'s only consumer.
    const lines = gitTooltipLines(signals({ dirty: true, upstream_gone: true }));
    expect(lines[0]).toBe("Uncommitted changes");
    expect(lines[1]).toContain("origin/feat-x is gone");
  });

  it("says nothing at all for a clean, fully pushed branch", () => {
    // No glyph renders, so no tooltip opens; there is nothing to say either.
    expect(gitTooltipLines(signals())).toEqual([]);
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

  /**
   * The line survives the glyph's removal, and is now reachable only alongside
   * another fact — a dirty tree, or an activity glyph holding the slot. Worth
   * keeping: "the branch you pushed to is gone" is the most useful sentence about
   * such a checkout, and it still never asserts the merge outright.
   */
  it("glosses a deleted upstream without asserting the merge", () => {
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

  /**
   * Guards a shape the daemon does not produce — `upstream_gone` is only ever set
   * after the upstream name is known non-empty — so this pins the fallback rather
   * than a reachable case. Kept because the fallback is one `??` and the cost of
   * it being wrong is the word "null" in a tooltip.
   */
  it("never leaves an upstream name as the literal null", () => {
    expect(
      gitTooltipLines(signals({ upstream: null, upstream_gone: true })).join(""),
    ).toContain("its upstream is gone");
  });
});

/**
 * The drift guard. `gitTooltipLines` and `gitDescription` are projections of one
 * fact list, and this asserts they stay projections — that they report the same
 * *number* of facts for every combination the daemon can emit.
 *
 * Worth a property test rather than more examples because the hand-maintained
 * versions drifted three times in review, each time by one clause, each time under
 * a comment claiming the sets matched. An example test only catches the omission
 * somebody thought of.
 */
describe("the tooltip and the description cannot drift apart", () => {
  const dirties: (boolean | null)[] = [null, false, true];
  // The five upstream shapes `parse_upstream_track` can produce — see its tests.
  const upstreams: Partial<WorktreeGitSignals>[] = [
    { upstream: null, ahead: null, behind: null }, // never pushed
    { upstream: "origin/x", ahead: null, behind: null, upstream_gone: true }, // gone
    { upstream: "origin/x", ahead: 0, behind: 0 }, // in sync
    { upstream: "origin/x", ahead: 3, behind: 2 }, // diverged
    { upstream: "origin/x", ahead: null, behind: null }, // unrecognised track token
  ];

  /**
   * The length comparison below counts by splitting on `", "`, so it is only sound
   * while no short form contains that sequence. Asserted rather than assumed —
   * otherwise a future fact worded "ahead, behind" would silently inflate the count
   * and make the drift guard pass while the sets diverged, which is the exact
   * failure this whole block exists to catch.
   */
  it("keeps short forms free of the separator the guard counts by", () => {
    for (const dirty of dirties) {
      for (const up of upstreams) {
        const description = gitDescription(signals({ dirty, ...up }));
        if (description === undefined) continue;
        for (const clause of description.split(", ")) {
          expect(clause).not.toContain(", ");
        }
      }
    }
  });

  it("reports the same facts in both registers, for every emittable shape", () => {
    for (const dirty of dirties) {
      for (const up of upstreams) {
        const git = signals({ dirty, ...up });
        const lines = gitTooltipLines(git);
        const description = gitDescription(git);
        // The description is additionally gated on a glyph rendering; past that
        // gate it must account for every line the tooltip has.
        if (description === undefined) continue;
        expect(description.split(", ")).toHaveLength(lines.length);
      }
    }
  });

  /** The specific shape that slipped through two fix rounds. */
  it("includes the no-upstream fact in both", () => {
    const git = signals({ dirty: true, upstream: null, ahead: null, behind: null });
    expect(gitTooltipLines(git)).toHaveLength(2);
    expect(gitDescription(git)).toBe("uncommitted changes, no upstream");
  });
});

describe("gitDescription", () => {
  it("is undefined when no glyph renders, so the row adds no clause", () => {
    expect(gitDescription(undefined)).toBeUndefined();
    expect(gitDescription(signals({ dirty: null }))).toBeUndefined();
    expect(gitDescription(signals())).toBeUndefined();
  });

  /** The combinations two review rounds found uncovered, and the daemon emits. */
  it("describes every fact, not only the one the glyph shows", () => {
    expect(gitDescription(signals({ dirty: true, ahead: 3, behind: 2 }))).toBe(
      "uncommitted changes, 3 commits not pushed, 2 commits behind",
    );
    expect(
      gitDescription(signals({ dirty: true, ahead: null, behind: null, upstream_gone: true })),
    ).toBe("uncommitted changes, upstream branch deleted");
    expect(gitDescription(signals({ dirty: true, behind: 4 }))).toBe(
      "uncommitted changes, 4 commits behind",
    );
  });

  it("describes each state in words for a screen reader", () => {
    expect(gitDescription(signals({ dirty: true }))).toBe("uncommitted changes");
    expect(gitDescription(signals({ ahead: 2 }))).toBe("2 commits not pushed");
    expect(gitDescription(signals({ ahead: 1 }))).toBe("1 commit not pushed");
    // No glyph for a clean branch or a deleted upstream, so no clause for either.
    expect(gitDescription(signals())).toBeUndefined();
    expect(
      gitDescription(signals({ ahead: null, behind: null, upstream_gone: true })),
    ).toBeUndefined();
  });
});
