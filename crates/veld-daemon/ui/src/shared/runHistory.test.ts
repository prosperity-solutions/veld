import { describe, expect, it } from "vitest";

import type { EnvironmentList, HistoryEntry, RunInfo } from "../api";
import {
  countHidden,
  hiddenByHorizon,
  horizonCutoff,
  pruneRunHistory,
  runEndedAt,
} from "./runHistory";

const NOW = new Date("2026-08-05T12:00:00Z");
const daysAgo = (n: number) =>
  new Date(NOW.getTime() - n * 24 * 60 * 60 * 1000).toISOString();

function run(over: Partial<RunInfo> = {}): RunInfo {
  return {
    name: "dev",
    status: "stopped",
    live: false,
    run_id: "r1",
    short_id: "r1",
    urls: {},
    nodes: [],
    created_at: "2026-08-05T00:00:00Z",
    ...over,
  };
}

function entry(over: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    run_id: "h1",
    short_id: "h1",
    status: "stopped",
    created_at: daysAgo(1),
    nodes: [],
    ...over,
  };
}

function envs(runs: RunInfo[]): EnvironmentList {
  return { projects: [{ name: "p", project_root: "/p", runs }] };
}

describe("horizonCutoff", () => {
  it("treats zero and negatives as off", () => {
    // Zero is the daemon's off-switch value for this key; a client that read it as
    // "hide everything" would empty the History tab for every default install.
    expect(horizonCutoff(0, NOW)).toBeNull();
    expect(horizonCutoff(-3, NOW)).toBeNull();
    expect(horizonCutoff(NaN, NOW)).toBeNull();
  });

  it("counts back whole days from the clock it is given", () => {
    expect(horizonCutoff(2, NOW)).toBe(Date.parse(daysAgo(2)));
  });
});

describe("runEndedAt", () => {
  it("prefers ended_at and falls back to created_at", () => {
    expect(runEndedAt({ ended_at: daysAgo(1), created_at: daysAgo(9) })).toBe(
      Date.parse(daysAgo(1)),
    );
    expect(runEndedAt({ created_at: daysAgo(9) })).toBe(Date.parse(daysAgo(9)));
  });

  it("returns null for a missing or unparseable timestamp", () => {
    expect(runEndedAt({})).toBeNull();
    expect(runEndedAt({ ended_at: "" })).toBeNull();
    expect(runEndedAt({ ended_at: "not a date" })).toBeNull();
  });

  it("parses the daemon's actual wire format", () => {
    // `veld_core::db::ts_to_str` emits RFC3339 with a `Z` and **microsecond**
    // precision. The other tests in this file build stamps with `toISOString()`, which
    // is milliseconds — so without this case nothing here would notice that the format
    // the daemon really sends had stopped parsing, and the horizon would silently treat
    // every run as undated (i.e. never hidden).
    expect(runEndedAt({ ended_at: "2026-08-05T08:00:00.123456Z" })).toBe(
      Date.parse("2026-08-05T08:00:00.123Z"),
    );
  });
});

describe("pruneRunHistory", () => {
  it("drops history entries past the horizon and keeps the rest", () => {
    const before = envs([
      run({ history: [entry({ run_id: "new", created_at: daysAgo(1) }), entry({ run_id: "old", created_at: daysAgo(5) })] }),
    ]);
    const after = pruneRunHistory(before, 2, NOW);
    expect(after.projects[0].runs[0].history!.map((h) => h.run_id)).toEqual(["new"]);
  });

  it("keeps an entry whose timestamp cannot be read", () => {
    // An unreadable clock is not evidence of age, and dropping the entry would take
    // its logs out of the picker with no way to ask for them back.
    const before = envs([run({ history: [entry({ created_at: "" })] })]);
    expect(pruneRunHistory(before, 1, NOW).projects[0].runs[0].history).toHaveLength(1);
  });

  it("never touches the run itself, only its history list", () => {
    // The load-bearing property for IDE mode: the pane is pointed at this run, and
    // hiding it would remove the logs and node states the pane exists to show.
    const before = envs([run({ ended_at: daysAgo(30), history: [entry({ created_at: daysAgo(30) })] })]);
    const after = pruneRunHistory(before, 1, NOW);
    expect(after.projects[0].runs).toHaveLength(1);
    expect(after.projects[0].runs[0].ended_at).toBe(daysAgo(30));
    expect(after.projects[0].runs[0].history).toEqual([]);
  });

  it("returns the input unchanged when the horizon is off", () => {
    const before = envs([run({ history: [entry({ created_at: daysAgo(99) })] })]);
    // Identity, not a deep copy: every poll runs this, and re-creating the payload
    // when nothing is filtered would re-render every card on every tick.
    expect(pruneRunHistory(before, 0, NOW)).toBe(before);
  });

  it("leaves a run with no history field alone", () => {
    const before = envs([run()]);
    expect(pruneRunHistory(before, 1, NOW).projects[0].runs[0].history).toBeUndefined();
  });
});

describe("hiddenByHorizon", () => {
  it("hides an ended run past the horizon", () => {
    expect(hiddenByHorizon(run({ ended_at: daysAgo(5) }), 2, NOW)).toBe(true);
    expect(hiddenByHorizon(run({ ended_at: daysAgo(1) }), 2, NOW)).toBe(false);
  });

  it("never hides a live run, however old", () => {
    // A long-running dev server started last week is not history. This is also what
    // keeps the Active tab identical under every setting.
    expect(hiddenByHorizon(run({ live: true, ended_at: daysAgo(30) }), 1, NOW)).toBe(false);
  });

  it("shows a run with no timestamp", () => {
    expect(hiddenByHorizon(run(), 1, NOW)).toBe(false);
  });
});

describe("countHidden", () => {
  it("counts what the horizon removes, ignoring live runs", () => {
    const rows = [
      run({ ended_at: daysAgo(5) }),
      run({ ended_at: daysAgo(6) }),
      run({ live: true, ended_at: daysAgo(9) }),
      run({ ended_at: daysAgo(1) }),
    ];
    expect(countHidden(rows, 2, NOW)).toBe(2);
    expect(countHidden(rows, 0, NOW)).toBe(0);
  });

  it("counts history entries too", () => {
    expect(countHidden([entry({ created_at: daysAgo(4) })], 1, NOW)).toBe(1);
  });
});
