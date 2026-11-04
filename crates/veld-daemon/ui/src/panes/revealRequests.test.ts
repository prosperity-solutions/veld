import { describe, expect, it } from "vitest";
import { dispatchRevealPane, onRevealPaneRequest, type RevealPaneRequest } from "./revealRequests";

function collect(msg: Record<string, unknown>): { ok: boolean; got: RevealPaneRequest[] } {
  const got: RevealPaneRequest[] = [];
  const off = onRevealPaneRequest((r) => got.push(r));
  const ok = dispatchRevealPane(msg);
  off();
  return { ok, got };
}

describe("dispatchRevealPane", () => {
  it("passes a tab, a resume, or just the worktree", () => {
    expect(collect({ worktree_id: 3, session_id: "s1" }).got).toEqual([
      { worktreeId: 3, sessionId: "s1" },
    ]);
    expect(collect({ worktree_id: 3, resume: { spec: "claude", session: "tok" } }).got).toEqual([
      { worktreeId: 3, resume: { spec: "claude", session: "tok" } },
    ]);
    expect(collect({ worktree_id: 3 }).got).toEqual([{ worktreeId: 3 }]);
  });

  it("drops a frame that does not parse", () => {
    for (const bad of [
      { worktree_id: "3" },
      { worktree_id: 1.5 },
      { worktree_id: 3, session_id: "" },
      { worktree_id: 3, session_id: 7 },
      { worktree_id: 3, resume: { spec: "claude" } },
      { worktree_id: 3, resume: "claude" },
    ]) {
      const { ok, got } = collect(bad);
      expect(ok).toBe(false);
      expect(got).toEqual([]);
    }
  });
});
