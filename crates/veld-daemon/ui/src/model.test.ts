import { describe, expect, it } from "vitest";
import type { EnvironmentList, RunInfo, Worktree } from "./api";
import {
  activeRun,
  filterWorktrees,
  runsForWorktree,
  sortedUrls,
  worktreeStatus,
} from "./model";

const wt = (path: string): Worktree => ({
  id: 1,
  repo_root: "/repo",
  path,
  branch: "feat/checkout-v2",
  alias: "chk",
  is_main: false,
  created_at: "2026-01-01T00:00:00Z",
  has_veld_config: true,
  presets: [],
});

const run = (
  name: string,
  status: RunInfo["status"],
  live = status !== "stopped" && status !== "failed",
): RunInfo => ({
  name,
  status,
  live,
  urls: {},
  nodes: [],
});

describe("runsForWorktree", () => {
  it("joins by worktree path === project_root", () => {
    const envs: EnvironmentList = {
      projects: [
        { name: "a", project_root: "/wts/chk", runs: [run("chk", "running")] },
        { name: "b", project_root: "/other", runs: [run("x", "running")] },
      ],
    };
    expect(runsForWorktree(envs, wt("/wts/chk")).map((r) => r.name)).toEqual([
      "chk",
    ]);
    expect(runsForWorktree(envs, wt("/nope"))).toEqual([]);
    expect(runsForWorktree(null, wt("/wts/chk"))).toEqual([]);
  });
});

describe("activeRun / worktreeStatus", () => {
  it("prefers running over transitional over stopped", () => {
    expect(
      activeRun([run("a", "stopped"), run("b", "starting"), run("c", "running")])
        ?.name,
    ).toBe("c");
    expect(activeRun([run("a", "stopped"), run("b", "starting")])?.name).toBe(
      "b",
    );
    expect(activeRun([run("a", "stopped")])).toBeNull();
    expect(activeRun([])).toBeNull();
  });

  it("maps to the rail dot states", () => {
    expect(worktreeStatus([run("a", "running")])).toBe("running");
    expect(worktreeStatus([run("a", "starting")])).toBe("partial");
    expect(worktreeStatus([run("a", "failed", true)])).toBe("partial");
    expect(worktreeStatus([run("a", "stopped")])).toBe("stopped");
    expect(worktreeStatus([])).toBe("stopped");
  });

  it("ignores non-live history runs", () => {
    // An environment's latest run persists as history (live: false) — a
    // crashed run must not read as active.
    expect(activeRun([run("a", "failed", false)])).toBeNull();
    expect(worktreeStatus([run("a", "failed", false)])).toBe("stopped");
    expect(
      activeRun([run("a", "failed", false), run("b", "running")])?.name,
    ).toBe("b");
  });
});

describe("sortedUrls", () => {
  it("sorts by service name", () => {
    const r = run("a", "running");
    r.urls = { frontend: "https://f", admin: "https://a" };
    expect(sortedUrls(r)).toEqual([
      ["admin", "https://a"],
      ["frontend", "https://f"],
    ]);
    expect(sortedUrls(null)).toEqual([]);
  });
});

describe("filterWorktrees", () => {
  it("matches alias and branch, case-insensitively", () => {
    const list = [wt("/a"), { ...wt("/b"), alias: "main", branch: "main" }];
    expect(filterWorktrees(list, "CHECKOUT")).toHaveLength(1);
    expect(filterWorktrees(list, "main")).toHaveLength(1);
    expect(filterWorktrees(list, "")).toHaveLength(2);
  });
});
