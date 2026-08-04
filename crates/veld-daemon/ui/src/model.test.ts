import { describe, expect, it } from "vitest";
import type { EnvironmentList, RunInfo, Worktree } from "./api";
import {
  activeRun,
  bestFuzzyMatch,
  diagnosticsRun,
  fuzzyMatch,
  prunePending,
  runSignature,
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
  emoji: "🦊",
  is_main: false,
  created_at: "2026-01-01T00:00:00Z",
  has_veld_config: true,
  presets: [],
  nodes: [],
  ide: { quicklinks: [], permissions: [] },
});

const run = (
  name: string,
  status: RunInfo["status"],
  live = status !== "stopped" && status !== "failed",
): RunInfo => ({
  name,
  status,
  live,
  run_id: `${name}-run-id`,
  short_id: name,
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
    expect(worktreeStatus([run("a", "failed", true)])).toBe("failed");
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

describe("diagnosticsRun", () => {
  it("is the active run when there is one", () => {
    expect(
      diagnosticsRun([run("a", "stopped"), run("b", "running")])?.name,
    ).toBe("b");
  });

  it("keeps the ended runs activeRun drops, so their logs stay readable", () => {
    // The whole reason this predicate exists: once a run has ended there is
    // nothing to stop or restart (activeRun → null) but the logs and the last
    // node states are exactly what the user is looking for, and
    // /api/logs/{run} serves an ended run's output.
    const stopped = run("dev", "stopped", true);
    expect(activeRun([stopped])).toBeNull();
    expect(diagnosticsRun([stopped])?.name).toBe("dev");

    const crashedHistory = run("dev", "failed", false);
    expect(activeRun([crashedHistory])).toBeNull();
    expect(diagnosticsRun([crashedHistory])?.name).toBe("dev");
  });

  it("prefers the environment holding the live slot over a history row", () => {
    const history = run("old", "failed", false);
    const liveSlot = run("dev", "stopped", true);
    expect(diagnosticsRun([history, liveSlot])?.name).toBe("dev");
  });

  it("falls back to the first listed run, and to null with none", () => {
    expect(diagnosticsRun([run("a", "failed", false)])?.name).toBe("a");
    expect(diagnosticsRun([])).toBeNull();
  });
});

describe("runSignature", () => {
  it("changes when a restart swaps the run id but not the status", () => {
    // The regression this exists for: `veld restart` tears down and starts a
    // fresh run, so a quick restart reads running → running. A status-only
    // check never observes it and the pending spinner runs to its timeout.
    const before = run("dev", "running");
    const after = { ...run("dev", "running"), run_id: "a-different-uuid" };
    expect(runSignature([before])).not.toBe(runSignature([after]));
  });

  it("changes across start and stop", () => {
    const stopped = runSignature([]);
    const starting = runSignature([run("dev", "starting")]);
    const running = runSignature([run("dev", "running")]);
    expect(new Set([stopped, starting, running]).size).toBe(3);
  });

  it("is stable while nothing happens", () => {
    const runs = [run("dev", "running")];
    expect(runSignature(runs)).toBe(runSignature([...runs]));
  });

  it("collapses non-live history to the same value as no runs at all", () => {
    // History must not read as an in-flight action.
    expect(runSignature([run("dev", "failed", false)])).toBe(runSignature([]));
  });
});

describe("prunePending", () => {
  const marker = (sig: string, expiresAt = 10_000) => ({
    label: "start" as const,
    sigAtSet: sig,
    expiresAt,
  });

  it("keeps a marker whose action has not landed yet", () => {
    const cur = { 1: marker("stopped") };
    expect(prunePending(cur, 0, () => "stopped")).toBe(cur);
  });

  it("drops a marker once the signature moves", () => {
    const cur = { 1: marker("stopped") };
    expect(prunePending(cur, 0, () => "running:abc")).toEqual({});
  });

  it("drops a marker for a worktree that no longer exists", () => {
    // It can never report a transition, so it would spin until the TTL.
    expect(prunePending({ 1: marker("stopped") }, 0, () => null)).toEqual({});
  });

  it("expires a marker whose action never produced a transition", () => {
    const cur = { 1: marker("stopped", 5_000) };
    expect(prunePending(cur, 4_999, () => "stopped")).toBe(cur);
    expect(prunePending(cur, 5_000, () => "stopped")).toEqual({});
  });

  it("prunes per worktree, leaving the others alone", () => {
    const cur = { 1: marker("stopped"), 2: marker("running:x") };
    const next = prunePending(cur, 0, (id) =>
      id === 1 ? "running:new" : "running:x",
    );
    expect(Object.keys(next)).toEqual(["2"]);
  });

  it("returns the SAME object when nothing changed", () => {
    // Load-bearing: the caller runs this from a useState updater inside an
    // effect, so a fresh object every poll would loop forever.
    const cur = { 1: marker("stopped") };
    expect(prunePending(cur, 0, () => "stopped")).toBe(cur);
    const empty = {};
    expect(prunePending(empty, 0, () => null)).toBe(empty);
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

const score = (text: string, query: string) => fuzzyMatch(text, query)?.score;

describe("fuzzyMatch", () => {
  it("matches subsequences, case-insensitively", () => {
    expect(fuzzyMatch("checkout-v2", "CHECKOUT")).not.toBeNull();
    expect(fuzzyMatch("checkout-v2", "ckv")).not.toBeNull();
    expect(fuzzyMatch("checkout-v2", "zz")).toBeNull();
    // Order matters — a subsequence, not a bag of characters.
    expect(fuzzyMatch("checkout", "kc")).toBeNull();
  });

  it("matches everything on an empty query, at score 0", () => {
    expect(fuzzyMatch("anything", "")).toEqual({ score: 0, positions: [] });
    expect(fuzzyMatch("anything", "   ")).toEqual({ score: 0, positions: [] });
  });

  it("treats spaces as separators, not characters to find", () => {
    expect(fuzzyMatch("New worktree…", "new wt")).not.toBeNull();
  });

  it("reports the matched positions for highlighting", () => {
    // c-h-e-c-k-o-u-t: the second 'c' is skipped, the scan is leftmost-greedy.
    expect(fuzzyMatch("checkout", "cko")?.positions).toEqual([0, 4, 5]);
  });

  it("ranks consecutive runs above scattered hits", () => {
    expect(score("checkout", "chk")!).toBeLessThan(score("checkout", "che")!);
  });

  it("ranks word-boundary hits above mid-word ones", () => {
    // 'v' starts a segment in the first, sits inside a word in the second.
    expect(score("checkout-v2", "v")!).toBeGreaterThan(score("review", "v")!);
  });

  it("never loses a match that is genuinely a subsequence", () => {
    // The property the boundary anchor must not break. Anchoring the first
    // character forward can strand the rest of the query past the boundary
    // it jumped to ("switch to veld-web" + `wt` anchors `w` to "-(w)eb",
    // after which there is no `t`) — so the anchored scan is an alternative
    // to the plain leftmost scan, never a replacement. A mis-ranked item is
    // still findable by scrolling; an unmatched one vanishes from ⌘K.
    const labels = [
      "main",
      "desktop-app-2",
      "Switch to veld-web",
      "Copy path of api",
      "Copy all run URLs",
      "Rename api…",
      "Collapse the worktree rail",
      "Change emoji for chk…",
      "New worktree…",
      "Remove project veld…",
      "feat/api-v2",
    ];
    const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789-";
    const isSubsequence = (text: string, query: string) => {
      let i = 0;
      for (const c of text.toLowerCase()) if (c === query[i]) i++;
      return i === query.length;
    };

    // Named cases the reviewers hit: typing the middle of a word is an
    // ordinary palette query, and each of these returned null under a
    // replace-the-scan anchor.
    expect(fuzzyMatch("Copy path of main", "opy")).not.toBeNull();
    expect(fuzzyMatch("feat/deploy-prod", "ploy")).not.toBeNull();
    expect(fuzzyMatch("Switch to veld-web", "wt")).not.toBeNull();
    expect(fuzzyMatch("Rename api…", "am")).not.toBeNull();

    const lost: string[] = [];
    for (const label of labels) {
      for (const a of alphabet) {
        for (const b of alphabet) {
          for (const c of alphabet) {
            for (const q of [a + b, a + b + c]) {
              if (!isSubsequence(label, q)) continue;
              if (fuzzyMatch(label, q) === null) lost.push(`${label} + ${q}`);
            }
          }
        }
      }
    }
    expect(lost).toEqual([]);
  });

  it("anchors the first character to a word start when one exists", () => {
    // Regression: plain greedy took the 'w' in "s(w)itch" and ranked
    // "Switch to Runs" ABOVE "New worktree…" for the query `wt` — the
    // acronym-ish shape most palette queries have.
    expect(score("New worktree…", "wt")!).toBeGreaterThan(
      score("Switch to Runs", "wt")!,
    );
    // The anchor applies to the first character only; later ones stay greedy.
    expect(fuzzyMatch("new worktree", "wt")?.positions).toEqual([4, 8]);
  });

  it("breaks ties toward the shorter, earlier-matching haystack", () => {
    expect(score("main", "main")!).toBeGreaterThan(
      score("main-experiment", "main")!,
    );
    expect(score("api", "api")!).toBeGreaterThan(score("legacy-api", "api")!);
  });
});

describe("bestFuzzyMatch", () => {
  it("takes the highest-scoring haystack", () => {
    // 'main' hits the branch, not the alias.
    const m = bestFuzzyMatch(["chk", "feat/main-rewrite"], "main");
    expect(m).not.toBeNull();
    expect(m!.score).toBe(fuzzyMatch("feat/main-rewrite", "main")!.score);
  });

  it("is null only when no haystack matches", () => {
    expect(bestFuzzyMatch(["chk", "feat/x"], "zzz")).toBeNull();
    expect(bestFuzzyMatch([], "a")).toBeNull();
  });
});
