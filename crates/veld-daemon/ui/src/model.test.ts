import { describe, expect, it } from "vitest";
import type { EnvironmentList, Lane, RunInfo, RunStatus, Worktree } from "./api";
import {
  activeRun,
  attentionStatus,
  bestFuzzyMatch,
  bulkMoveTargets,
  bulkTrashable,
  detachedInSection,
  diagnosticsRun,
  freshRunName,
  fuzzyMatch,
  laneDropTarget,
  liveRuns,
  MAIN_LANE,
  moveLane,
  moveWorktree,
  needsAttention,
  parsePendingKey,
  pendingKey,
  pickRun,
  proposeRunName,
  prunePending,
  railGroups,
  runSignature,
  runSignatureFor,
  runStatus,
  runsForWorktree,
  sectionAttention,
  selectorRuns,
  siblingRuns,
  sortRunsForDisplay,
  sortedUrls,
  spinnerAction,
  startRunName,
  transitionAction,
  UNGROUPED_LABEL,
  worktreeStatus,
  worstStatus,
  DELETING_LANE,
  DETACHED_LANE,
  TRASH_LANE,
} from "./model";

const wt = (path: string): Worktree => ({
  id: 1,
  repo_root: "/repo",
  path,
  branch: "feat/checkout-v2",
  alias: "chk",
  display_name: "",
  emoji: "🦊",
  marker_color: "#008cff",
  is_main: false,
  created_at: "2026-01-01T00:00:00Z",
  lane: "",
  sort_position: null,
  trashed_at: "",
  trash_error: "",
  deleting: false,
  has_veld_config: true,
  presets: [],
  nodes: [],
  machine_vars: 0,
  ide: { quicklinks: [], permissions: [], panes: [] },
});

const run = (
  name: string,
  status: RunInfo["status"],
  live = status !== "stopped" && status !== "failed",
  created_at = "2026-01-01T00:00:00Z",
  ended_at: string | null =
    status === "stopped" || status === "failed" ? "2026-01-01T01:00:00Z" : null,
): RunInfo => ({
  name,
  status,
  live,
  run_id: `${name}-run-id`,
  short_id: name,
  created_at,
  ended_at,
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

  it("reduces a run status to what a surface renders", () => {
    expect(worktreeStatus([run("a", "running")])).toBe("running");
    expect(worktreeStatus([run("a", "starting")])).toBe("partial");
    expect(worktreeStatus([run("a", "stopping")])).toBe("partial");
    expect(worktreeStatus([run("a", "failed", true)])).toBe("failed");
    expect(worktreeStatus([run("a", "stopped")])).toBe("stopped");
    expect(worktreeStatus([])).toBe("stopped");
  });

  it("keeps recovering out of partial", () => {
    // Folded into `partial` it rendered as a spinner, which reads as
    // "perpetually starting" for a node the monitor is restarting on a loop.
    // It routes to the attention affordance instead, like `failed`.
    expect(worktreeStatus([run("a", "recovering")])).toBe("recovering");
    expect(transitionAction(run("a", "recovering"))).toBeNull();
  });

  it("agrees with transitionAction on exactly which statuses are partial", () => {
    // The rail draws a spinner when `transitionAction` names a direction, and
    // `partial` is what that state used to be called. If the two ever disagree
    // one of them is rendering a state the other does not believe in — a
    // spinner with no colour, or a transition with no spinner.
    const all: RunStatus[] = [
      "starting",
      "running",
      "recovering",
      "stopping",
      "stopped",
      "failed",
    ];
    for (const status of all) {
      const r = run("a", status, true);
      expect(
        transitionAction(r) !== null,
        `${status}: partial=${worktreeStatus([r])} action=${transitionAction(r)}`,
      ).toBe(worktreeStatus([r]) === "partial");
    }
    // And the direction is the one the spinner's colour is keyed on.
    expect(transitionAction(run("a", "starting"))).toBe("start");
    expect(transitionAction(run("a", "stopping"))).toBe("stop");
    expect(transitionAction(null)).toBeNull();
  });

  it("spinnerAction prefers the local marker over the observed transition", () => {
    // The ordering is the only thing that knows a restart was ONE action rather
    // than a stop followed by a start — which is what keeps the top bar's spinner
    // on the button that was pressed.
    expect(spinnerAction("restart", run("a", "stopping"))).toBe("restart");
    expect(spinnerAction("stop", run("a", "starting"))).toBe("stop");
    // No local marker: the observed transition drives it. This is the case every
    // run control missed before — a run started from the CLI or another window.
    expect(spinnerAction(null, run("a", "starting"))).toBe("start");
    expect(spinnerAction(null, run("a", "stopping"))).toBe("stop");
    // Nothing moving, and nothing to spin for.
    expect(spinnerAction(null, run("a", "running"))).toBeNull();
    expect(spinnerAction(null, run("a", "recovering"))).toBeNull();
    expect(spinnerAction(null, null)).toBeNull();
  });

  it("no OBSERVED state both spins and asks for attention", () => {
    // The complement matters as much as the set: anything `needsAttention`
    // rejects has to be representable by a run control on its own, or the state
    // has no surface at all.
    //
    // Scoped to the *observed* status on purpose — `spinnerAction(null, …)`. What
    // the daemon reports is one state, so it may light one channel; a row showing
    // both would mean the two-signal collision this change removed had come back
    // through the status itself. The local-marker case is deliberately different
    // and is pinned separately below.
    const all: RunStatus[] = [
      "starting",
      "running",
      "recovering",
      "stopping",
      "stopped",
      "failed",
    ];
    for (const status of all) {
      const s = worktreeStatus([run("a", status, true)]);
      const attention = needsAttention(s);
      expect(attention, `${status} -> ${s}`).toBe(
        s === "failed" || s === "recovering",
      );
      expect(
        attention && spinnerAction(null, run("a", status, true)) !== null,
        `${status} is both spinning and alerting`,
      ).toBe(false);
    }
    expect(needsAttention(worktreeStatus([]))).toBe(false);
  });

  it("but a local marker DOES coexist with attention, and must", () => {
    // Stopping a failed run has always been offered (`running` is
    // `status !== "stopped"`, unchanged), so a row can carry the alert and a
    // spinner at once between the click and the next poll. That is not the
    // collision this change removed: the alert reports the *run's* state and the
    // spinner reports *my action on it*, they are different shapes in different
    // columns, and only one of them is transient. Asserted rather than assumed,
    // because the test above reads as forbidding it — and a guard that claims more
    // than the code holds is worse than no guard.
    const failed = run("a", "failed", true);
    expect(needsAttention(worktreeStatus([failed]))).toBe(true);
    expect(spinnerAction("stop", failed)).toBe("stop");
    const recovering = run("a", "recovering");
    expect(needsAttention(worktreeStatus([recovering]))).toBe(true);
    expect(spinnerAction("restart", recovering)).toBe("restart");
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

describe("pickRun", () => {
  it("binds to the stored name, not to the daemon's ordering", () => {
    // The defect this exists for: two `running` runs meant the alphabetically
    // first name won silently, and the other had no dot, no logs, no stop.
    const runs = [run("api", "running"), run("web", "running")];
    expect(activeRun(runs)?.name).toBe("api");
    expect(pickRun(runs, "web").run?.name).toBe("web");
    expect(pickRun(runs, "web").missing).toBeNull();
  });

  it("binds to an ENDED run when that is what was chosen", () => {
    // Logs and last node states outlive the run, and "what happened to the one
    // I was watching" is the question right after it dies.
    const runs = [run("api", "running"), run("web", "failed")];
    expect(pickRun(runs, "web").run?.name).toBe("web");
  });

  it("reports a vanished choice instead of silently swapping it", () => {
    const runs = [run("api", "running")];
    const pick = pickRun(runs, "gone");
    expect(pick.run?.name).toBe("api");
    // `missing` is what the caller renders — a fallback presented under the old
    // name is how a run vanishes from under a reader mid-glance.
    expect(pick.missing).toBe("gone");
  });

  it("falls back to the diagnostics run with no stored choice", () => {
    const runs = [run("api", "running")];
    expect(pickRun(runs, null).run).toBe(diagnosticsRun(runs));
    expect(pickRun(runs, null).missing).toBeNull();
    expect(pickRun([], null).run).toBeNull();
  });
});

describe("selectorRuns", () => {
  it("hides ended environments while something is live", () => {
    // An entry is an environment, not a run, so a directory accumulates the
    // names you started last week. Those are Runs mode's business.
    const runs = [run("api", "running"), run("old", "stopped"), run("dead", "failed")];
    const { runs: shown, hidden } = selectorRuns(runs, "api");
    expect(shown.map((r) => r.name)).toEqual(["api"]);
    expect(hidden).toBe(2);
  });

  it("shows everything when nothing is live", () => {
    // "The run crashed overnight" is the case the app is opened for; hiding the
    // only entry there is would strand its logs and last node states.
    const runs = [run("api", "failed"), run("old", "stopped")];
    const { runs: shown, hidden } = selectorRuns(runs, undefined);
    expect(shown).toHaveLength(2);
    expect(hidden).toBe(0);
  });

  it("always lists the bound environment, even ended", () => {
    // Hiding the entry the control is NAMING is the vanishing behaviour this
    // change exists to remove.
    const runs = [run("api", "running"), run("old", "stopped")];
    const { runs: shown, hidden } = selectorRuns(runs, "old");
    expect(shown.map((r) => r.name)).toEqual(["api", "old"]);
    expect(hidden).toBe(0);
  });

  it("reveals everything on request, keeping the daemon's order", () => {
    const runs = [run("api", "running"), run("old", "stopped")];
    const { runs: shown, hidden } = selectorRuns(runs, "api", true);
    expect(shown).toBe(runs);
    expect(hidden).toBe(0);
  });

  it("hides nothing when there is nothing to hide", () => {
    expect(selectorRuns([], undefined)).toEqual({ runs: [], hidden: 0 });
    const live = [run("api", "starting")];
    expect(selectorRuns(live, "api").hidden).toBe(0);
  });
});

describe("sortRunsForDisplay", () => {
  it("puts live runs newest-started first", () => {
    const runs = sortRunsForDisplay([
      run("a", "running", true, "2026-01-01T00:02:00Z"),
      run("c", "running", true, "2026-01-01T00:03:00Z"),
      run("b", "running", true, "2026-01-01T00:01:00Z"),
    ]);
    expect(runs.map((r) => r.name)).toEqual(["c", "a", "b"]);
  });

  it("puts ended runs last-stopped first", () => {
    const runs = sortRunsForDisplay([
      run("a", "stopped", false, "2026-01-01T00:00:00Z", "2026-01-01T00:05:00Z"),
      run("b", "failed", false, "2026-01-01T00:00:00Z", "2026-01-01T00:06:00Z"),
      run("c", "stopped", false, "2026-01-01T00:00:00Z", "2026-01-01T00:04:00Z"),
    ]);
    expect(runs.map((r) => r.name)).toEqual(["b", "a", "c"]);
  });

  it("keeps the live group ahead of ended runs whatever the times", () => {
    const runs = sortRunsForDisplay([
      // Ended long ago but after the live run started.
      run("old", "stopped", false, "2026-01-01T00:00:00Z", "2026-01-01T00:07:00Z"),
      run("live", "running", true, "2026-01-01T00:08:00Z"),
    ]);
    expect(runs.map((r) => r.name)).toEqual(["live", "old"]);
  });

  it("is deterministic on full ties (name, then run_id)", () => {
    // Same start and end on both — the comparator must still pick a fixed order
    // so a poll refresh never reshuffles the list.
    const a = run("z", "stopped", false, "2026-01-01T00:00:00Z", "2026-01-01T00:01:00Z");
    const b = run("a", "stopped", false, "2026-01-01T00:00:00Z", "2026-01-01T00:01:00Z");
    expect(sortRunsForDisplay([a, b]).map((r) => r.name)).toEqual(["a", "z"]);
    expect(sortRunsForDisplay([b, a]).map((r) => r.name)).toEqual(["a", "z"]);
  });

  it("does not mutate its input", () => {
    const input: RunInfo[] = [run("b", "stopped"), run("a", "running")];
    const before = input.map((r) => r.name);
    sortRunsForDisplay(input);
    expect(input.map((r) => r.name)).toEqual(before);
  });
});

describe("worstStatus", () => {
  it("reports the run that needs attention, not the healthiest one", () => {
    // The inverse of `activeRun`'s order, on purpose: this answers "does
    // anything here need looking at", so a failed sibling must outrank a
    // running one or the badge hides the case it exists for.
    expect(worstStatus([run("a", "running"), run("b", "failed")])).toBe("failed");
    expect(worstStatus([run("a", "running"), run("b", "recovering")])).toBe(
      "recovering",
    );
    expect(worstStatus([run("a", "running"), run("b", "starting")])).toBe(
      "partial",
    );
    expect(worstStatus([])).toBe("stopped");
  });

  it("agrees with runStatus for a single run", () => {
    for (const s of ["running", "starting", "stopping", "failed", "recovering"] as const) {
      expect(worstStatus([run("a", s)])).toBe(runStatus(run("a", s)));
    }
  });
});

describe("attentionStatus", () => {
  it("says nothing about a worktree that is fine", () => {
    expect(attentionStatus([])).toBeNull();
    expect(attentionStatus([run("a", "running")])).toBeNull();
    expect(attentionStatus([run("a", "starting")])).toBeNull();
  });

  it("reports the picked run when that is the thing to look at", () => {
    expect(attentionStatus([run("a", "failed", true)])).toBe("failed");
    expect(attentionStatus([run("a", "recovering")])).toBe("recovering");
  });

  /**
   * The case the row asked two questions to catch, and the reason this reduction
   * is shared rather than copied: `worktreeStatus` reports the *picked* run, and
   * the picker prefers a healthy one — so a second environment that failed in the
   * same directory had no representation at all while the first stayed green.
   */
  it("reports a live sibling that failed while the picked run is green", () => {
    expect(attentionStatus([run("api", "running"), run("web", "failed", true)])).toBe(
      "failed",
    );
  });

  /** History is not attention. A failed run that is no longer live is the record
   *  of something that already ended; the row it sits on has nothing to open. */
  it("ignores a failed run that is only history", () => {
    expect(attentionStatus([run("api", "running"), run("web", "failed")])).toBeNull();
  });
});

describe("sectionAttention", () => {
  const envsFor = (byPath: Record<string, RunInfo[]>): EnvironmentList => ({
    projects: Object.entries(byPath).map(([project_root, runs]) => ({
      name: project_root,
      project_root,
      runs,
    })),
  });
  const at = (path: string, over: Partial<Worktree> = {}): Worktree => ({
    ...wt(path),
    ...over,
  });

  it("says nothing about an empty section, or one where nothing is wrong", () => {
    expect(sectionAttention(null, [])).toBeNull();
    expect(
      sectionAttention(envsFor({ "/a": [run("x", "running")] }), [at("/a")]),
    ).toBeNull();
  });

  /**
   * Worst wins, by the same severity order a single row uses between its own
   * runs. A header that reported the first row it found, or the commonest state,
   * would let a failure hide behind a section that is merely busy — which is the
   * one thing a folded section must not do.
   */
  it("reports the most attention-worthy worktree in the section", () => {
    const envs = envsFor({
      "/a": [run("x", "running")],
      "/b": [run("y", "recovering")],
      "/c": [run("z", "failed", true)],
    });
    expect(sectionAttention(envs, [at("/a"), at("/b"), at("/c")])).toBe("failed");
    expect(sectionAttention(envs, [at("/a"), at("/b")])).toBe("recovering");
  });

  /** Trashed rows are silent, the same exclusion the row and the project badge
   *  make: that directory is on its way off the disk, so an alert about it
   *  offers nothing to do. A folded trash therefore reports a count and nothing
   *  else. */
  it("ignores trashed worktrees", () => {
    const envs = envsFor({ "/a": [run("x", "failed", true)] });
    expect(
      sectionAttention(envs, [at("/a", { trashed_at: "2026-01-02T00:00:00Z" })]),
    ).toBeNull();
  });

  it("says nothing when there is no environment data yet", () => {
    expect(sectionAttention(null, [at("/a")])).toBeNull();
  });
});

describe("liveRuns / siblingRuns", () => {
  it("separates live runs from history and the bound run from its siblings", () => {
    const runs = [run("api", "running"), run("web", "stopped"), run("db", "starting")];
    expect(liveRuns(runs).map((r) => r.name)).toEqual(["api", "db"]);
    expect(siblingRuns(runs, "api").map((r) => r.name)).toEqual(["web", "db"]);
    // No bound run: everything is an alternative.
    expect(siblingRuns(runs, undefined)).toHaveLength(3);
  });
});

describe("freshRunName", () => {
  it("avoids history as well as live names", () => {
    // `proposeRunName` may return a stopped environment's name, which is right for
    // ▶ ("run that again") and wrong for an action labelled *another* — it would
    // name an environment already sitting in the list.
    const runs = [run("chk", "stopped"), run("chk-2", "failed")];
    expect(proposeRunName("chk", runs)).toBe("chk");
    expect(freshRunName("chk", runs)).toBe("chk-3");
  });

  it("returns the alias when the worktree has no runs at all", () => {
    expect(freshRunName("chk", [])).toBe("chk");
  });
});

describe("startRunName", () => {
  it("suffixes rather than colliding with a live environment", () => {
    // ▶ used to send no name, so the daemon defaulted to the alias: with an
    // agent's run live that minted a third environment, and with the alias
    // itself live it replaced that run.
    expect(startRunName("chk", [], null)).toBe("chk");
    expect(startRunName("chk", [run("chk", "running")], null)).toBe("chk-2");
    expect(
      startRunName("chk", [run("chk", "running"), run("chk-2", "starting")], null),
    ).toBe("chk-3");
  });

  it("ignores stopped environments — starting one again is the point", () => {
    expect(startRunName("chk", [run("chk", "stopped")], null)).toBe("chk");
    expect(proposeRunName("chk", [run("chk", "failed")])).toBe("chk");
  });

  it("is NOT the same question as 'start another one'", () => {
    // ▶ means "run that again" and the explicit *another* action means "a fresh
    // environment". With a crashed `dev` bound next to a live `api` the two
    // answers differ, and sharing one helper made the menu entry labelled
    // "Start another run" re-run `dev` instead.
    const dev = run("dev", "failed");
    const runs = [run("api", "running"), dev];
    expect(startRunName("chk", runs, dev)).toBe("dev");
    expect(proposeRunName("chk", runs)).toBe("chk");
  });

  it("re-runs the SELECTED environment when it has ended", () => {
    // Selecting last night's crashed `dev` and pressing ▶ must continue that
    // environment's history, not open `dev-2` next to it.
    const dev = run("dev", "failed");
    expect(startRunName("chk", [dev], dev)).toBe("dev");
    // A live selection is not re-run: that is what ■ and restart are for.
    const live = run("dev", "running");
    expect(startRunName("chk", [live], live)).toBe("chk");
  });
});

describe("runSignatureFor", () => {
  it("watches ONE environment, so a sibling's transition cannot clear it", () => {
    const before = [run("api", "starting"), run("web", "running")];
    const after = [run("api", "starting"), run("web", "stopping")];
    expect(runSignatureFor(before, "api")).toBe(runSignatureFor(after, "api"));
    expect(runSignatureFor(before, "web")).not.toBe(runSignatureFor(after, "web"));
  });

  it("is 'none' for a name with no environment yet — a start in flight", () => {
    expect(runSignatureFor([], "api")).toBe("none");
  });
});

describe("prunePending", () => {
  const marker = (sig: string, expiresAt = 10_000) => ({
    label: "start" as const,
    sigAtSet: sig,
    expiresAt,
  });

  it("keeps a marker whose action has not landed yet", () => {
    const cur = { [pendingKey(1, "dev")]: marker("stopped") };
    expect(prunePending(cur, 0, () => "stopped")).toBe(cur);
  });

  it("drops a marker once the signature moves", () => {
    const cur = { [pendingKey(1, "dev")]: marker("stopped") };
    expect(prunePending(cur, 0, () => "running:abc")).toEqual({});
  });

  it("drops a marker for a worktree that no longer exists", () => {
    // It can never report a transition, so it would spin until the TTL.
    expect(
      prunePending({ [pendingKey(1, "dev")]: marker("stopped") }, 0, () => null),
    ).toEqual({});
  });

  it("expires a marker whose action never produced a transition", () => {
    const cur = { [pendingKey(1, "dev")]: marker("stopped", 5_000) };
    expect(prunePending(cur, 4_999, () => "stopped")).toBe(cur);
    expect(prunePending(cur, 5_000, () => "stopped")).toEqual({});
  });

  it("prunes per marker, leaving the others alone", () => {
    const cur = {
      [pendingKey(1, "dev")]: marker("stopped"),
      [pendingKey(2, "dev")]: marker("running:x"),
    };
    const next = prunePending(cur, 0, (key) =>
      parsePendingKey(key)?.worktreeId === 1 ? "running:new" : "running:x",
    );
    expect(Object.keys(next)).toEqual([pendingKey(2, "dev")]);
  });

  it("tracks two runs of ONE worktree independently", () => {
    // The reason the key gained the environment name: with one slot per
    // worktree, stopping `api` while `web` was starting in the same directory
    // overwrote the first marker and stranded its spinner.
    const cur = {
      [pendingKey(7, "api")]: marker("running:a"),
      [pendingKey(7, "web")]: marker("starting:b"),
    };
    const next = prunePending(cur, 0, (key) =>
      parsePendingKey(key)?.runName === "api" ? "stopping:a" : "starting:b",
    );
    expect(Object.keys(next)).toEqual([pendingKey(7, "web")]);
  });

  it("returns the SAME object when nothing changed", () => {
    // Load-bearing: the caller runs this from a useState updater inside an
    // effect, so a fresh object every poll would loop forever.
    const cur = { [pendingKey(1, "dev")]: marker("stopped") };
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

// ---------------------------------------------------------------------------
// Rail grouping and drag ordering (§18/§19)
// ---------------------------------------------------------------------------

/** A worktree with just the fields the rail's grouping reads. */
const rw = (path: string, over: Partial<Worktree> = {}): Worktree => ({
  ...wt(path),
  alias: path.replace("/wts/", ""),
  ...over,
});

const lane = (name: string, position: number): Lane => ({
  repo_root: "/repo",
  name,
  position,
  created_at: "2026-01-01T00:00:00Z",
});

describe("railGroups", () => {
  it("gives the main checkout its own pinned section", () => {
    // Main is the repository, not one of the branches you are juggling, so it gets
    // a divider under it. Pinned: it always leads, so it takes no part in ordering.
    const groups = railGroups(
      [rw("/repo", { is_main: true }), rw("/wts/a")],
      [],
    );
    // The trash lane is always present and pinned last; these assertions are
    // about the live rail, so the always-on trash is filtered out.
    const live = groups.filter((g) => g.key !== TRASH_LANE && g.key !== DELETING_LANE);
    expect(live.map((g) => g.key)).toEqual([MAIN_LANE, ""]);
    expect(live[0].pinned).toBe(true);
    expect(live[0].worktrees.map((w) => w.path)).toEqual(["/repo"]);
    expect(live[1].pinned).toBe(false);
    expect(live[1].worktrees.map((w) => w.path)).toEqual(["/wts/a"]);
  });

  it("keeps main inside a lane the user assigned it to", () => {
    // Its own section is a default, not a rule — assigned to a lane it belongs
    // there, because that is where someone put it on purpose.
    const groups = railGroups(
      [rw("/repo", { is_main: true, lane: "review" })],
      [lane("review", 0)],
    );
    const live = groups.filter((g) => g.key !== TRASH_LANE && g.key !== DELETING_LANE);
    expect(live.map((g) => g.key)).toEqual(["", "review"]);
    expect(groups[1].worktrees.map((w) => w.path)).toEqual(["/repo"]);
  });

  it("distinguishes the main section from the ungrouped one by key, not lane", () => {
    // Both write `lane: ""`. Keying drop targets on `lane` made them the same
    // target, so a drop meant for the ungrouped list also lit main's section.
    const groups = railGroups([rw("/repo", { is_main: true }), rw("/wts/a")], []);
    const live = groups.filter((g) => g.key !== TRASH_LANE && g.key !== DELETING_LANE);
    expect(live.map((g) => g.lane)).toEqual(["", ""]);
    expect(new Set(live.map((g) => g.key)).size).toBe(2);
  });

  it("puts ungrouped worktrees first and keeps empty lanes", () => {
    const groups = railGroups(
      [rw("/wts/a"), rw("/wts/b", { lane: "review" })],
      [lane("review", 0), lane("spikes", 1)],
    );
    const live = groups.filter((g) => g.key !== TRASH_LANE && g.key !== DELETING_LANE);
    expect(live.map((g) => g.lane)).toEqual(["", "review", "spikes"]);
    expect(groups[0].label).toBe(UNGROUPED_LABEL);
    expect(groups[0].worktrees.map((w) => w.path)).toEqual(["/wts/a"]);
    expect(groups[1].worktrees.map((w) => w.path)).toEqual(["/wts/b"]);
    // An empty lane is kept: it is where you drop the first worktree into it.
    expect(groups[2].worktrees).toEqual([]);
    // The always-on trash sits last, after the lanes.
    expect(groups[groups.length - 1].key).toBe(TRASH_LANE);
  });

  it("offers a create button on exactly the sections a worktree can be created into", () => {
    // The rail's only create affordance lives in these headers, so a section
    // wrongly marked `addable: false` is a destination the user cannot reach —
    // and one wrongly marked `true` is a button whose click the daemon rejects.
    const groups = railGroups(
      [
        rw("/repo", { is_main: true }),
        rw("/wts/a"),
        rw("/wts/b", { lane: "review" }),
        rw("/wts/gone", { trashed_at: "2026-01-01T00:00:00Z" }),
        rw("/wts/going", { trashed_at: "2026-01-01T00:00:00Z", deleting: true }),
      ],
      [lane("review", 0)],
    );
    expect(
      groups.filter((g) => g.addable).map((g) => g.key),
    ).toEqual(["", "review"]);
    // Never the main checkout's own section, the trash, or a removal in flight.
    expect(groups.find((g) => g.key === MAIN_LANE)?.addable).toBe(false);
    expect(groups.find((g) => g.key === TRASH_LANE)?.addable).toBe(false);
    expect(groups.find((g) => g.key === DELETING_LANE)?.addable).toBe(false);
  });

  it("offers the lane menu only on a real lane", () => {
    // `editable` is deliberately not `!pinned`: the ungrouped section is not
    // pinned and does take drops, but there is no lane record behind it, so a ⋮
    // there would offer to rename and delete something that does not exist.
    const groups = railGroups(
      [rw("/repo", { is_main: true }), rw("/wts/a"), rw("/wts/b", { lane: "review" })],
      [lane("review", 0)],
    );
    expect(groups.filter((g) => g.editable).map((g) => g.key)).toEqual(["review"]);
    expect(groups.find((g) => g.key === "")?.editable).toBe(false);
    expect(groups.find((g) => g.key === "")?.pinned).toBe(false);
  });

  it("gives every section but the main checkout's a header", () => {
    // The header is where the create button lives, so a section with no label
    // renders no "＋" at all. Main is the deliberate exception: it holds exactly
    // one row and creating "into" it means nothing.
    const groups = railGroups(
      [rw("/repo", { is_main: true }), rw("/wts/a")],
      [lane("review", 0)],
    );
    expect(groups.filter((g) => g.label === null).map((g) => g.key)).toEqual([
      MAIN_LANE,
    ]);
  });

  it("preserves the daemon's order rather than re-sorting", () => {
    // The daemon already applies the manual order (WT_ORDER). Re-sorting here
    // would silently re-derive it from a different rule and undo the drag.
    const groups = railGroups(
      [rw("/wts/z"), rw("/wts/a"), rw("/wts/m")],
      [],
    );
    expect(groups[0].worktrees.map((w) => w.path)).toEqual([
      "/wts/z",
      "/wts/a",
      "/wts/m",
    ]);
  });

  it("renders a worktree whose lane no longer exists as ungrouped", () => {
    // Unreachable in practice — delete_lane clears assignments in the same
    // transaction — but a row the client cannot place is a row the user cannot
    // reach, which is the worse failure.
    const groups = railGroups([rw("/wts/a", { lane: "ghost" })], []);
    expect(groups[0].worktrees.map((w) => w.path)).toEqual(["/wts/a"]);
  });

  it("separates trashed worktrees into their own group, last", () => {
    const groups = railGroups(
      [
        rw("/wts/a"),
        rw("/wts/going", { trashed_at: "2026-01-01T00:00:00Z", lane: "review" }),
      ],
      [lane("review", 0)],
    );
    expect(groups.map((g) => g.lane)).toEqual(["", "review", TRASH_LANE]);
    // Out of its lane while it sits in the trash, even though the row still carries
    // it — the lane comes back when it is restored.
    expect(groups[1].worktrees).toEqual([]);
    expect(groups[2].worktrees.map((w) => w.path)).toEqual(["/wts/going"]);
    expect(groups[2].label).toBe("Trash");
  });

  it("keeps the trash group even when it is empty", () => {
    // Trash is the rail's permanent bottom anchor, so it is rendered as an empty
    // lane rather than hidden — always a place that means "trash exists".
    const groups = railGroups([rw("/wts/a")], []);
    const trash = groups.find((g) => g.key === TRASH_LANE);
    expect(trash).toBeDefined();
    expect(trash?.label).toBe("Trash");
    expect(trash?.pinned).toBe(true);
    expect(trash?.worktrees).toEqual([]);
    // ...and it is still last.
    expect(groups[groups.length - 1].key).toBe(TRASH_LANE);
  });

  it("pulls actively-deleting worktrees into their own terminal lane", () => {
    const deleting = rw("/wts/going", {
      trashed_at: "2026-01-01T00:00:00Z",
      deleting: true,
    });
    const stillTrashed = rw("/wts/still", {
      trashed_at: "2026-01-01T00:00:00Z",
    });
    const groups = railGroups([rw("/wts/a"), deleting, stillTrashed], []);
    expect(groups.map((g) => g.key)).toEqual(["", DELETING_LANE, TRASH_LANE]);
    const del = groups.find((g) => g.key === DELETING_LANE)!;
    expect(del.label).toBe("Deleting");
    expect(del.pinned).toBe(true);
    expect(del.worktrees.map((w) => w.path)).toEqual(["/wts/going"]);
    // The deleting row is out of the trash while its removal runs.
    const trash = groups.find((g) => g.key === TRASH_LANE)!;
    expect(trash.worktrees.map((w) => w.path)).toEqual(["/wts/still"]);
  });

  it("omits the deleting lane when nothing is being deleted", () => {
    const groups = railGroups([rw("/wts/a")], []);
    expect(groups.some((g) => g.key === DELETING_LANE)).toBe(false);
  });

  it("groups detached checkouts into their own lane, between ungrouped and lanes", () => {
    // A detached HEAD is a state worth surfacing on its own, so detached
    // checkouts get a virtual lane of their own after the ungrouped worktrees
    // and before the real lanes.
    const groups = railGroups(
      [
        rw("/wts/a"),
        rw("/wts/det", { branch: "(detached)" }),
        rw("/wts/b", { lane: "review" }),
      ],
      [lane("review", 0)],
    );
    const live = groups.filter(
      (g) => g.key !== TRASH_LANE && g.key !== DELETING_LANE,
    );
    expect(live.map((g) => g.key)).toEqual(["", DETACHED_LANE, "review"]);
    const det = live.find((g) => g.key === DETACHED_LANE)!;
    expect(det.label).toBe("Detached");
    expect(det.pinned).toBe(true);
    // Pinned and not a drop target — you cannot file a checkout *as* detached.
    expect(det.addable).toBe(false);
    expect(det.editable).toBe(false);
    expect(det.worktrees.map((w) => w.path)).toEqual(["/wts/det"]);
  });

  it("pulls a detached checkout out of its lane while it is detached", () => {
    // Being detached overrides where the row belongs (same rule the trash
    // applies to trashed rows): it leaves the lane and returns when a branch is
    // checked out again.
    const groups = railGroups(
      [rw("/wts/det", { branch: "(detached)", lane: "review" })],
      [lane("review", 0)],
    );
    expect(groups.find((g) => g.key === DETACHED_LANE)?.worktrees).toHaveLength(1);
    expect(groups.find((g) => g.key === "review")?.worktrees).toHaveLength(0);
  });

  it("omits the detached lane when nothing is detached", () => {
    const groups = railGroups([rw("/wts/a")], []);
    expect(groups.some((g) => g.key === DETACHED_LANE)).toBe(false);
  });

  it("keeps a detached main checkout out of the detached lane", () => {
    // git keeps a repo's main on a branch, so a detached main is not a real
    // state — but the row must not silently disappear from the rail either.
    const groups = railGroups([rw("/repo", { is_main: true, branch: "(detached)" })], []);
    const live = groups.filter(
      (g) => g.key !== TRASH_LANE && g.key !== DELETING_LANE,
    );
    expect(live.map((g) => g.key)).toEqual([MAIN_LANE, ""]);
    expect(live[0].worktrees.map((w) => w.path)).toEqual(["/repo"]);
  });
});

describe("railGroups — batch actions", () => {
  it("offers the batch actions on the ungrouped section and on real lanes", () => {
    // `bulk` is not `editable`: the ungrouped section has no lane to rename or
    // delete, but it does hold a set of worktrees to move or bin — and in a repo
    // with no lanes it holds every checkout there is.
    const groups = railGroups(
      [
        rw("/repo", { is_main: true }),
        rw("/wts/a"),
        rw("/wts/det", { branch: "(detached)" }),
        rw("/wts/b", { lane: "review" }),
        rw("/wts/gone", { deleting: true, trashed_at: "2026-01-01T00:00:00Z" }),
        rw("/wts/bin", { trashed_at: "2026-01-01T00:00:00Z" }),
      ],
      [lane("review", 0)],
    );
    const bulk = new Map(groups.map((g) => [g.key, g.bulk]));
    expect(bulk.get("")).toBe(true);
    expect(bulk.get("review")).toBe(true);
    // Every pinned section says no, each for its own reason (one row and it is
    // the repo; own header button; a lane move would be invisible; leaving).
    expect(bulk.get(MAIN_LANE)).toBe(false);
    expect(bulk.get(DETACHED_LANE)).toBe(false);
    expect(bulk.get(DELETING_LANE)).toBe(false);
    expect(bulk.get(TRASH_LANE)).toBe(false);
    // The ungrouped section is the one that has `bulk` without `editable`, which
    // is the whole reason the two flags are separate.
    const ungrouped = groups.find((g) => g.key === "")!;
    expect(ungrouped.editable).toBe(false);
  });

  it("keys every section uniquely, even against lanes named like the sentinels", () => {
    // **The invariant `sectionMembers` actually needs**, and the one an earlier
    // `key === lane` assertion could not catch: that assertion held for a lane
    // called `main` too, while `railGroups` was handing the pinned main section
    // the literal key `"main"` — two sections, one key, and every lookup landing
    // on the pinned one. So a lane named after the default branch listed the main
    // checkout as its members and could never be trashed. The fixture names a lane
    // after **every** section key the rail produces, so a future virtual section
    // that picks a collidable key fails here rather than in someone's rail.
    const collidable = ["main", "trash", "deleting", "detached", "Worktrees"];
    const groups = railGroups(
      [
        rw("/repo", { id: 1, is_main: true }),
        rw("/wts/a", { id: 2 }),
        rw("/wts/det", { id: 3, branch: "(detached)" }),
        rw("/wts/gone", { id: 4, deleting: true, trashed_at: "2026-01-01T00:00:00Z" }),
        rw("/wts/bin", { id: 5, trashed_at: "2026-01-01T00:00:00Z" }),
        ...collidable.map((n, i) => rw(`/wts/lane-${i}`, { id: 10 + i, lane: n })),
      ],
      collidable.map((n, i) => lane(n, i)),
    );
    const keys = groups.map((g) => g.key);
    expect(new Set(keys).size).toBe(keys.length);
    // And the consequence that matters: a lane called `main` resolves to its own
    // members, not to the main checkout.
    const byKey = (k: string) => groups.find((g) => g.key === k)!;
    expect(byKey("main").worktrees.map((w) => w.path)).toEqual(["/wts/lane-0"]);
    expect(byKey(MAIN_LANE).worktrees.map((w) => w.path)).toEqual(["/repo"]);
    // `onLaneMenu` hands `group.lane` to a lookup that resolves by `key`, so the
    // two must still agree for every section that has a menu.
    for (const g of groups) {
      if (g.editable || g.bulk) expect(g.key).toBe(g.lane);
    }
  });
});

describe("bulkMoveTargets", () => {
  const three = [lane("review", 0), lane("wip", 1), lane("done", 2)];

  it("offers the other lanes and the ungrouped section, in rail order", () => {
    // Ungrouped leads because that is where it sits in the rail, and the labels
    // read top-to-bottom the same way the user is looking at them.
    expect(bulkMoveTargets(three, "wip")).toEqual([
      { value: "", label: "No group" },
      { value: "review", label: "review" },
      { value: "done", label: "done" },
    ]);
  });

  it("never offers the section the worktrees are already in", () => {
    expect(bulkMoveTargets(three, "wip").map((t) => t.value)).not.toContain("wip");
    // From ungrouped, the ungrouped option is the one that drops out.
    expect(bulkMoveTargets(three, "").map((t) => t.value)).toEqual([
      "review",
      "wip",
      "done",
    ]);
  });

  it("does not label the ungrouped option with the rail header it does not mean", () => {
    // Two reasons, both in `bulkMoveTargets`: `UNGROUPED_LABEL` is itself a legal
    // lane name (so a repo with a lane called "Worktrees" would offer two
    // byte-identical options), and — the sharper one — the main checkout does not
    // land under that header when it is ungrouped, it leads the rail in a pinned
    // section of its own. Naming the header would be wrong about exactly the row
    // a batch is most likely to surprise someone with.
    const targets = bulkMoveTargets([lane(UNGROUPED_LABEL, 0)], "other");
    expect(new Set(targets.map((t) => t.label)).size).toBe(targets.length);
    expect(targets.find((t) => t.value === "")?.label).not.toBe(UNGROUPED_LABEL);
  });

  it("is empty only for the ungrouped section of a repo with no lanes", () => {
    // The one case with no *existing* destination — and it does NOT disable the
    // gesture: the dialog also offers "New group…", so the repo that has never
    // defined a group can still file its whole rail into one. A *lane* always has
    // somewhere: ungrouping is a destination, so a repo's only lane still offers it.
    expect(bulkMoveTargets([], "")).toEqual([]);
    expect(bulkMoveTargets([lane("review", 0)], "review")).toEqual([
      { value: "", label: "No group" },
    ]);
  });
});

describe("detachedInSection", () => {
  const lanes = [lane("review", 0)];

  it("finds the detached rows a lane's batch will leave behind", () => {
    // They are filed into "review" but render under Detached, so the batch never
    // sees them — and their `lane` still says "review", so checking a branch out
    // again puts them back in a group the user emptied. Both dialogs say so.
    const wts = [
      rw("/wts/a", { id: 2, lane: "review" }),
      rw("/wts/det", { id: 3, lane: "review", branch: "(detached)" }),
    ];
    expect(railGroups(wts, lanes).find((g) => g.key === "review")!.worktrees)
      .toHaveLength(1);
    expect(detachedInSection(wts, lanes, "review").map((w) => w.path)).toEqual([
      "/wts/det",
    ]);
  });

  it("counts a dangling lane as ungrouped, the way railGroups does", () => {
    // A row whose lane no longer exists is ungrouped on the read path, so the
    // ungrouped section's batch is the one that would leave it behind.
    const wts = [rw("/wts/det", { id: 2, lane: "ghost", branch: "(detached)" })];
    expect(detachedInSection(wts, lanes, "review")).toEqual([]);
    expect(detachedInSection(wts, lanes, "").map((w) => w.path)).toEqual([
      "/wts/det",
    ]);
  });

  it("never counts the main checkout or a trashed row", () => {
    // Main leads the rail even while detached and is a member of nothing; a
    // trashed row is already out of every section.
    const wts = [
      rw("/repo", { id: 1, is_main: true, branch: "(detached)" }),
      rw("/wts/bin", {
        id: 2,
        branch: "(detached)",
        trashed_at: "2026-01-01T00:00:00Z",
      }),
    ];
    expect(detachedInSection(wts, lanes, "")).toEqual([]);
  });
});

describe("bulkTrashable", () => {
  it("never includes the main checkout", () => {
    // Main can be filed into a lane on purpose, so it does reach a lane's member
    // list — but binning it would take the repository with it.
    const members = [
      rw("/repo", { id: 1, is_main: true, lane: "review" }),
      rw("/wts/a", { id: 2, lane: "review" }),
    ];
    expect(bulkTrashable(members).map((w) => w.path)).toEqual(["/wts/a"]);
  });

  it("is empty for a lane holding only the main checkout", () => {
    // Which is what disables the menu entry: there is nothing there to bin.
    expect(bulkTrashable([rw("/repo", { is_main: true })])).toEqual([]);
  });
});

describe("moveWorktree", () => {
  const groups = () =>
    railGroups(
      [
        rw("/wts/a"),
        rw("/wts/b"),
        rw("/wts/x", { lane: "review" }),
        rw("/wts/y", { lane: "review" }),
      ],
      [lane("review", 0)],
    );

  it("reorders within a group and returns the full path order", () => {
    // Full list, not a delta: that is what makes the write idempotent, and paths
    // rather than ids because worktrees.id is a rowid SQLite reuses.
    expect(moveWorktree(groups(), "/wts/b", "", 0)).toEqual({
      lane: "",
      order: ["/wts/b", "/wts/a", "/wts/x", "/wts/y"],
    });
  });

  it("moves across lanes and reports the destination lane", () => {
    expect(moveWorktree(groups(), "/wts/a", "review", 1)).toEqual({
      lane: "review",
      order: ["/wts/b", "/wts/x", "/wts/a", "/wts/y"],
    });
  });

  it("clamps an index past the end of a list that just shrank", () => {
    // The index comes from a drop position, so a stale render can hand over one
    // past the end — the source row is removed before the insert.
    expect(moveWorktree(groups(), "/wts/a", "", 99)?.order).toEqual([
      "/wts/b",
      "/wts/a",
      "/wts/x",
      "/wts/y",
    ]);
  });

  it("refuses a drop into a pinned section", () => {
    expect(moveWorktree(groups(), "/wts/a", TRASH_LANE, 0)).toBeNull();
    const withMain = railGroups(
      [rw("/repo", { is_main: true }), rw("/wts/a")],
      [],
    );
    expect(moveWorktree(withMain, "/wts/a", MAIN_LANE, 0)).toBeNull();
  });

  it("can drop a worktree into a lane named after the default branch", () => {
    // The other half of the key collision `MAIN_LANE` fixes, and it predates the
    // batch actions: `moveWorktree` resolves its target by key, so while the
    // pinned main section held the literal key `"main"` a drop aimed at a lane
    // called `main` found that section first — pinned, so the move returned
    // `null` and the worktree simply would not go in.
    const groups = railGroups(
      [
        rw("/repo", { id: 1, is_main: true }),
        rw("/wts/a", { id: 2 }),
        rw("/wts/b", { id: 3, lane: "main" }),
      ],
      [lane("main", 0)],
    );
    const moved = moveWorktree(groups, "/wts/a", "main", 0);
    expect(moved).not.toBeNull();
    expect(moved!.lane).toBe("main");
    expect(moved!.order).toContain("/wts/a");
  });

  it("refuses a drop into a section that does not exist", () => {
    expect(moveWorktree(groups(), "/wts/a", "ghost", 0)).toBeNull();
  });

  it("leaves the main checkout out of the written order", () => {
    // Main needs no position — the daemon sorts `is_main DESC` first — and giving
    // it one would let a reorder of the branches move the repository.
    const withMain = railGroups(
      [rw("/repo", { is_main: true }), rw("/wts/a"), rw("/wts/b")],
      [],
    );
    expect(moveWorktree(withMain, "/wts/b", "", 0)?.order).toEqual([
      "/wts/b",
      "/wts/a",
    ]);
  });

  it("inserts at the top when dropped on a lane's header", () => {
    // The section-level fallback resolves to index 0 — the header and the padding
    // above it are what reach it, and both are at the top of the section.
    const g = railGroups(
      [rw("/wts/a", { lane: "spikes" }), rw("/wts/b", { lane: "spikes" })],
      [lane("spikes", 0)],
    );
    expect(moveWorktree(g, "/wts/b", "spikes", 0)?.order).toEqual([
      "/wts/b",
      "/wts/a",
    ]);
  });

  it("appends when dropped past the end of an empty lane", () => {
    // The empty-lane target reports index 0 (its length), and the write has to be
    // the destination lane with the worktree in it.
    const g = railGroups([rw("/wts/a")], [lane("spikes", 0)]);
    expect(moveWorktree(g, "/wts/a", "spikes", 0)).toEqual({
      lane: "spikes",
      order: ["/wts/a"],
    });
  });

  it("returns null for a path it cannot find", () => {
    expect(moveWorktree(groups(), "/wts/nope", "", 0)).toBeNull();
  });

  it("excludes trashed worktrees from the written order", () => {
    // They are on their way out, so a position would be written and then deleted.
    const g = railGroups(
      [rw("/wts/a"), rw("/wts/b"), rw("/wts/gone", { trashed_at: "t" })],
      [],
    );
    expect(moveWorktree(g, "/wts/b", "", 0)?.order).toEqual([
      "/wts/b",
      "/wts/a",
    ]);
  });
});

describe("laneDropTarget", () => {
  // Three sections stacked with a 9px gutter between them, as the rail renders
  // them: 0 spans 10-50, 1 spans 59-99, 2 spans 108-148.
  const sections = [
    { index: 0, bottom: 50 },
    { index: 1, bottom: 99 },
    { index: 2, bottom: 148 },
  ];

  it("aims at the lane the pointer is inside", () => {
    expect(laneDropTarget(sections, 30)).toBe(0);
    expect(laneDropTarget(sections, 70)).toBe(1);
    expect(laneDropTarget(sections, 120)).toBe(2);
  });

  it("gives everything above the first lane to the first lane", () => {
    // The ungrouped section and the list's own padding sit up there, and they
    // are not lane targets — but a pointer over them still has to mean
    // something, or "drag a lane to the top" is a gesture with nowhere to land.
    expect(laneDropTarget(sections, 0)).toBe(0);
    expect(laneDropTarget(sections, -400)).toBe(0);
  });

  it("gives everything below the last lane to the last lane", () => {
    // The dead zone that made the whole feature look one-directional: dragging
    // a lane to the bottom of the rail and letting go landed on nothing.
    expect(laneDropTarget(sections, 149)).toBe(2);
    expect(laneDropTarget(sections, 5000)).toBe(2);
  });

  it("gives a gutter to the lane under it", () => {
    // 51-58 is between two sections and belongs to neither element.
    expect(laneDropTarget(sections, 55)).toBe(1);
  });

  it("has nothing to aim at in a rail with no lanes", () => {
    expect(laneDropTarget([], 42)).toBeNull();
  });

  it("skips a section whose index did not parse", () => {
    // `data-lane-index` is read off the DOM, so a missing attribute arrives as
    // NaN — which must not become the answer, or every drop lands nowhere.
    expect(
      laneDropTarget([{ index: Number.NaN, bottom: 50 }, ...sections], 30),
    ).toBe(0);
    expect(laneDropTarget([{ index: Number.NaN, bottom: 50 }], 30)).toBeNull();
  });
});

describe("moveLane", () => {
  const lanes = () => [lane("a", 0), lane("b", 1), lane("c", 2)];

  it("takes the place of the lane it was dropped on, going up", () => {
    expect(moveLane(lanes(), "c", "b")).toEqual(["a", "c", "b"]);
    expect(moveLane(lanes(), "c", "a")).toEqual(["c", "a", "b"]);
  });

  it("takes the place of the lane it was dropped on, going down", () => {
    // The case the first implementation could not reach at all: an insertion
    // point past the dragged lane's own position resolved to where it already
    // sat, so one step down did nothing whichever half of "b" was released on.
    expect(moveLane(lanes(), "a", "b")).toEqual(["b", "a", "c"]);
    expect(moveLane(lanes(), "a", "c")).toEqual(["b", "c", "a"]);
  });

  it("is a full order, not a delta", () => {
    // `reorder_lanes` appends anything unmentioned, so a partial write would
    // silently move every lane the drag did not touch.
    expect(moveLane(lanes(), "b", "a")).toEqual(["b", "a", "c"]);
  });

  it("is one step per click for the ⋮ menu, said the same way", () => {
    // The menu names the neighbour, exactly as a drop onto it would.
    expect(moveLane(lanes(), "b", "a")).toEqual(["b", "a", "c"]);
    expect(moveLane(lanes(), "b", "c")).toEqual(["a", "c", "b"]);
  });

  it("reports a drop on the lane itself as null", () => {
    // The normal way to abandon a lane drag, and it must not cost a request and
    // a refresh.
    expect(moveLane(lanes(), "b", "b")).toBeNull();
  });

  it("returns null when either lane is unknown", () => {
    // A stale render: the lane was renamed or deleted by another window between
    // this drag starting and the drop.
    expect(moveLane(lanes(), "ghost", "a")).toBeNull();
    expect(moveLane(lanes(), "a", "ghost")).toBeNull();
  });

  it("has nothing to do with a single lane", () => {
    expect(moveLane([lane("only", 0)], "only", "only")).toBeNull();
  });

  it("moves two lanes past each other in both directions", () => {
    // The case that read as completely dead: with two lanes there is only one
    // neighbour, so a defect in either direction removes half the feature.
    const two = [lane("a", 0), lane("b", 1)];
    expect(moveLane(two, "a", "b")).toEqual(["b", "a"]);
    expect(moveLane(two, "b", "a")).toEqual(["b", "a"]);
  });

  it("moves a lane across several places in one go", () => {
    // A drag is not limited to a neighbour, which is the whole reason it exists
    // beside a menu that steps one at a time.
    const four = [lane("a", 0), lane("b", 1), lane("c", 2), lane("d", 3)];
    expect(moveLane(four, "d", "a")).toEqual(["d", "a", "b", "c"]);
    expect(moveLane(four, "a", "d")).toEqual(["b", "c", "d", "a"]);
  });
});
