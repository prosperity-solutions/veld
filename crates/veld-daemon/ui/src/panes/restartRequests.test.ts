import { describe, expect, it } from "vitest";
import { dispatchRestartPane, onRestartPaneRequest, type RestartPaneRequest } from "./restartRequests";

const frame = {
  type: "restart_pane",
  worktree_id: 7,
  session_id: "tab-1",
  mode: "resume",
  prompt: "carry on",
};

describe("dispatchRestartPane", () => {
  it("hands a well-formed frame to subscribers", () => {
    const got: RestartPaneRequest[] = [];
    const stop = onRestartPaneRequest((r) => got.push(r));
    expect(dispatchRestartPane(frame)).toBe(true);
    stop();
    expect(got).toEqual([{ worktreeId: 7, sessionId: "tab-1", mode: "resume", prompt: "carry on" }]);
  });

  it("treats a missing prompt as empty", () => {
    const got: RestartPaneRequest[] = [];
    const stop = onRestartPaneRequest((r) => got.push(r));
    dispatchRestartPane({ ...frame, prompt: undefined, mode: "fresh" });
    stop();
    expect(got[0]).toMatchObject({ mode: "fresh", prompt: "" });
  });

  it("drops frames it cannot trust", () => {
    const got: RestartPaneRequest[] = [];
    const stop = onRestartPaneRequest((r) => got.push(r));
    expect(dispatchRestartPane({ ...frame, mode: "shell" })).toBe(false);
    expect(dispatchRestartPane({ ...frame, session_id: "" })).toBe(false);
    expect(dispatchRestartPane({ ...frame, worktree_id: "7" })).toBe(false);
    stop();
    expect(got).toEqual([]);
  });
});
