import { describe, expect, it } from "vitest";

import type { WorktreeGitSignals } from "../api";
import type { RowState, RowSummary } from "../inbox/inbox";
import { rowBadge, rowDescription, rowTooltip, seenNote } from "./rowState";

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

function summary(state: RowState | null, entries = 0, running = 0, seen = 0): RowSummary {
  return {
    state,
    entries: Array.from({ length: entries }, (_, i) => ({
      sessionId: `s${i}`,
      unseen: { detail: `pane ${i} finished` } as RowSummary["entries"][number]["unseen"],
    })),
    running,
    seen,
  };
}

describe("rowBadge", () => {
  it("shows nothing with no shell and no activity", () => {
    expect(rowBadge(summary(null), 0)).toBeNull();
  });

  it("is the idle dot for a shell with nothing to report", () => {
    expect(rowBadge(summary(null), 2)).toEqual({ kind: "idle" });
  });

  it("carries the activity state over a merely running shell", () => {
    expect(rowBadge(summary("working", 0, 1), 1)).toEqual({ kind: "activity", state: "working" });
    for (const state of ["attention", "failed", "finished", "opened"] as RowState[]) {
      expect(rowBadge(summary(state, 1), 3)).toEqual({ kind: "activity", state });
    }
  });

  /**
   * Unread news outlives the shell that produced it: a command that finished in
   * a pane since closed is still worth the badge.
   */
  it("shows activity even when no shell is left running", () => {
    expect(rowBadge(summary("finished", 1), 0)).toEqual({ kind: "activity", state: "finished" });
  });

  /**
   * A finished agent that was looked at is still finished: the muted tick, over the
   * grey dot it says more than, and under any activity, which is news.
   */
  it("shows a seen finish between activity and the idle dot", () => {
    expect(rowBadge(summary(null, 0, 0, 1), 2)).toEqual({ kind: "seen" });
    expect(rowBadge(summary(null, 0, 0, 1), 0)).toEqual({ kind: "seen" });
    expect(rowBadge(summary("working", 0, 1, 1), 1)).toEqual({ kind: "activity", state: "working" });
    expect(rowBadge(summary("finished", 1, 0, 1), 1)).toEqual({
      kind: "activity",
      state: "finished",
    });
  });
});

describe("seenNote", () => {
  it("names seen finishes, and nothing without any", () => {
    expect(seenNote(0)).toBeUndefined();
    expect(seenNote(1)).toBe("Agent finished (seen)");
    expect(seenNote(3)).toBe("3 agents finished (seen)");
  });
});

describe("rowTooltip", () => {
  it("is the bare label when there is nothing to say", () => {
    expect(rowTooltip("api", [], undefined)).toBe("api");
    expect(rowTooltip("api", [], signals())).toBe("api");
  });

  /**
   * **The glyph collapses; the tooltip does not.** This is what makes sharing a
   * slot a presentation choice rather than a loss of information — a spinning row
   * still tells you it has unpushed commits.
   */
  it("carries activity and git in one tooltip", () => {
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
    expect(rowDescription(undefined, undefined)).toBeUndefined();
    expect(rowDescription(undefined, signals())).toBeUndefined();
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

  it("ends with the live-terminal clause, which can stand alone", () => {
    expect(rowDescription("working", signals({ ahead: 2 }), false, "2 terminals running")).toBe(
      "working. 2 commits not pushed. 2 terminals running",
    );
    expect(rowDescription(undefined, undefined, false, "1 terminal running")).toBe(
      "1 terminal running",
    );
  });
});
