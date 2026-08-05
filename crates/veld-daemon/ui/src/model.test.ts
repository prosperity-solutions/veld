import { describe, expect, it } from "vitest";
import type { EnvironmentList, Lane, RunInfo, RunStatus, Worktree } from "./api";
import {
  activeRun,
  bestFuzzyMatch,
  diagnosticsRun,
  freshRunName,
  fuzzyMatch,
  liveRuns,
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
  selectorRuns,
  siblingRuns,
  sortedUrls,
  spinnerAction,
  startRunName,
  transitionAction,
  worktreeStatus,
  worstStatus,
  TRASH_LANE,
} from "./model";

const wt = (path: string): Worktree => ({
  id: 1,
  repo_root: "/repo",
  path,
  branch: "feat/checkout-v2",
  alias: "chk",
  emoji: "🦊",
  marker_color: "#008cff",
  is_main: false,
  created_at: "2026-01-01T00:00:00Z",
  lane: "",
  sort_position: null,
  trashed_at: "",
  trash_error: "",
  has_veld_config: true,
  config_parsed: true,
  presets: [],
  nodes: [],
  ide: { quicklinks: [], permissions: [], panes: [] },
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

  it("prunes per marker, leaving the others alone", () => {
    const cur = { 1: marker("stopped"), 2: marker("running:x") };
    const next = prunePending(cur, 0, (key) =>
      key === "1" ? "running:new" : "running:x",
    );
    expect(Object.keys(next)).toEqual(["2"]);
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
    expect(groups.map((g) => g.key)).toEqual(["main", ""]);
    expect(groups[0].pinned).toBe(true);
    expect(groups[0].worktrees.map((w) => w.path)).toEqual(["/repo"]);
    expect(groups[1].pinned).toBe(false);
    expect(groups[1].worktrees.map((w) => w.path)).toEqual(["/wts/a"]);
  });

  it("keeps main inside a lane the user assigned it to", () => {
    // Its own section is a default, not a rule — assigned to a lane it belongs
    // there, because that is where someone put it on purpose.
    const groups = railGroups(
      [rw("/repo", { is_main: true, lane: "review" })],
      [lane("review", 0)],
    );
    expect(groups.map((g) => g.key)).toEqual(["", "review"]);
    expect(groups[1].worktrees.map((w) => w.path)).toEqual(["/repo"]);
  });

  it("distinguishes the main section from the ungrouped one by key, not lane", () => {
    // Both write `lane: ""`. Keying drop targets on `lane` made them the same
    // target, so a drop meant for the ungrouped list also lit main's section.
    const groups = railGroups([rw("/repo", { is_main: true }), rw("/wts/a")], []);
    expect(groups.map((g) => g.lane)).toEqual(["", ""]);
    expect(new Set(groups.map((g) => g.key)).size).toBe(2);
  });

  it("puts ungrouped worktrees first and keeps empty lanes", () => {
    const groups = railGroups(
      [rw("/wts/a"), rw("/wts/b", { lane: "review" })],
      [lane("review", 0), lane("spikes", 1)],
    );
    expect(groups.map((g) => g.lane)).toEqual(["", "review", "spikes"]);
    expect(groups[0].label).toBeNull();
    expect(groups[0].worktrees.map((w) => w.path)).toEqual(["/wts/a"]);
    expect(groups[1].worktrees.map((w) => w.path)).toEqual(["/wts/b"]);
    // An empty lane is kept: it is where you drop the first worktree into it.
    expect(groups[2].worktrees).toEqual([]);
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

  it("omits the trash group entirely when the trash is empty", () => {
    const groups = railGroups([rw("/wts/a")], []);
    expect(groups.map((g) => g.lane)).toEqual([""]);
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
    expect(moveWorktree(withMain, "/wts/a", "main", 0)).toBeNull();
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
