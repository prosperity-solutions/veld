import { describe, expect, it } from "vitest";

import type { WorktreeGitSignals } from "../api";
import type { RowState, RowSummary } from "../inbox/inbox";
import { rowDescription, rowGlyph, rowTooltip } from "./rowState";

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

function summary(state: RowState | null, entries = 0, running = 0): RowSummary {
  return {
    state,
    entries: Array.from({ length: entries }, (_, i) => ({
      sessionId: `s${i}`,
      unseen: { detail: `pane ${i} finished` } as RowSummary["entries"][number]["unseen"],
    })),
    running,
  };
}

describe("rowGlyph", () => {
  it("shows nothing when neither half has anything", () => {
    expect(rowGlyph(summary(null), signals())).toBeNull();
    expect(rowGlyph(summary(null), undefined)).toBeNull();
  });

  it("shows git state when there is no activity", () => {
    expect(rowGlyph(summary(null), signals({ dirty: true }))).toEqual({
      kind: "git",
      state: "dirty",
    });
  });

  it("shows activity when there is no git state", () => {
    expect(rowGlyph(summary("attention", 1), signals())).toEqual({
      kind: "activity",
      state: "attention",
    });
  });

  /**
   * **The reason these share a slot.** While an agent is running the tree is
   * *expected* to be dirty, so a pencil beside a spinner reports something the
   * spinner already implies.
   */
  it("lets a working agent outrank uncommitted changes", () => {
    expect(rowGlyph(summary("working", 0, 2), signals({ dirty: true }))).toEqual({
      kind: "activity",
      state: "working",
    });
  });

  it("lets every activity state outrank every git state", () => {
    const states: RowState[] = ["attention", "failed", "finished", "working"];
    const gits: WorktreeGitSignals[] = [
      signals({ dirty: true }),
      signals({ ahead: 4 }),
      signals({ ahead: null, behind: null, upstream_gone: true }),
    ];
    for (const state of states) {
      for (const git of gits) {
        expect(rowGlyph(summary(state, 1), git)).toEqual({ kind: "activity", state });
      }
    }
  });

  /**
   * `working` is opt-in (Settings → the rail's activity glyph). With it off the
   * inbox reports no state, so the git glyph correctly takes the slot back rather
   * than the row going blank.
   */
  it("falls back to git state when the inbox reports nothing", () => {
    expect(rowGlyph(summary(null), signals({ ahead: 2 }))).toEqual({
      kind: "git",
      state: "unpushed",
    });
  });
});

describe("rowTooltip", () => {
  it("is the bare label when there is nothing to say", () => {
    expect(rowTooltip("api", [], signals())).toBe("api");
    expect(rowTooltip("api", [], undefined)).toBe("api");
  });

  /**
   * **The glyph collapses; the tooltip does not.** This is what makes sharing a
   * slot a presentation choice rather than a loss of information — a spinning row
   * still tells you it has unpushed commits.
   */
  it("carries both halves even though only one glyph renders", () => {
    expect(
      rowTooltip("api", ["2 unseen", "pane 0 finished"], signals({ ahead: 3 })),
    ).toBe("api — 2 unseen\npane 0 finished\n3 commits not pushed to origin/feat-x");
  });

  it("puts activity first, because that is what changed", () => {
    const lines = rowTooltip("api", ["1 pane is running something"], signals({ dirty: true }));
    expect(lines.indexOf("running")).toBeLessThan(lines.indexOf("Uncommitted"));
  });

  it("reads exactly as before when only one half has facts", () => {
    expect(rowTooltip("api", ["waiting for you"], signals())).toBe("api — waiting for you");
    expect(rowTooltip("api", [], signals({ dirty: true }))).toBe("api — Uncommitted changes");
  });
});

describe("rowDescription", () => {
  it("is undefined when the row has no state at all", () => {
    expect(rowDescription(undefined, signals())).toBeUndefined();
    expect(rowDescription(undefined, undefined)).toBeUndefined();
  });

  /**
   * The glyph is `aria-hidden`, so this is the whole non-visual account of the
   * row. A reader told an agent is working but not that the checkout has unpushed
   * commits gets strictly less than a sighted reader who hovers.
   */
  it("announces both halves, not just the one the glyph shows", () => {
    expect(rowDescription("working", signals({ ahead: 2 }))).toBe(
      "working. 2 commits not pushed",
    );
  });

  it("announces whichever half exists on its own", () => {
    expect(rowDescription("working", signals())).toBe("working");
    expect(rowDescription(undefined, signals({ dirty: true }))).toBe("uncommitted changes");
  });
});
