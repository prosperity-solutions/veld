import { describe, expect, it } from "vitest";

import {
  PALETTE_SLOTS,
  assignSlots,
  axisMax,
  contiguousRuns,
  stackBands,
  stackedMax,
} from "./TimeSeriesChart";
import type { ChartSeries } from "./TimeSeriesChart";
import { STACK_METRICS, bucketValue, fmtCpuTime, fmtPercent } from "../ResourcePanel";
import type { StatsBucket } from "../../api";

function bucket(over: Partial<StatsBucket> = {}): StatsBucket {
  return {
    t: 0,
    samples: 1,
    cpu: 0,
    cpu_peak: 0,
    procs: 1,
    resident: 1000,
    footprint: 500,
    footprint_peak: 500,
    virtual: 9000,
    private_clean: 10,
    private_dirty: 20,
    shared_clean: 30,
    shared_dirty: 40,
    swap: 50,
    wired: 60,
    ...over,
  };
}

describe("contiguousRuns", () => {
  it("splits at gaps so a chart never joins across missing samples", () => {
    // Present at 0,1 — absent at 2 — present at 3,4.
    const present = (i: number) => i !== 2;
    expect(contiguousRuns(5, present)).toEqual([
      [0, 1],
      [3, 4],
    ]);
  });

  it("handles gaps at both ends and a fully absent series", () => {
    expect(contiguousRuns(4, (i) => i === 1 || i === 2)).toEqual([[1, 2]]);
    expect(contiguousRuns(3, () => false)).toEqual([]);
    expect(contiguousRuns(0, () => true)).toEqual([]);
  });

  it("keeps a lone present point as its own run", () => {
    // Regression guard: a single sample must still be drawable, not silently
    // dropped for having no neighbour to connect to.
    expect(contiguousRuns(3, (i) => i === 1)).toEqual([[1]]);
  });
});

describe("axisMax", () => {
  it("rounds up to a readable bound", () => {
    expect(axisMax(0)).toBe(1);
    expect(axisMax(83)).toBe(100);
    expect(axisMax(120)).toBe(150);
    expect(axisMax(1024)).toBe(1500);
  });

  it("never returns a bound below the data", () => {
    for (const v of [1, 7, 99, 101, 4096, 123456789]) {
      expect(axisMax(v)).toBeGreaterThanOrEqual(v);
    }
  });

  it("treats a negative or zero max as 1 rather than dividing by zero", () => {
    expect(axisMax(-5)).toBe(1);
  });
});

describe("bucketValue", () => {
  it("maps the three totals to their own fields", () => {
    const b = bucket();
    expect(bucketValue(b, "footprint")).toBe(500);
    expect(bucketValue(b, "resident")).toBe(1000);
    expect(bucketValue(b, "virtual")).toBe(9000);
  });

  it("reads page classes straight through", () => {
    const b = bucket();
    expect(bucketValue(b, "private_dirty")).toBe(20);
    expect(bucketValue(b, "swap")).toBe(50);
  });

  it("preserves null for a class the platform cannot report", () => {
    // Not 0: an unmeasurable class must render as unavailable, since a
    // zero-height band claims the node holds none of that memory.
    const b = bucket({ private_dirty: null });
    expect(bucketValue(b, "private_dirty")).toBeNull();
  });
});

describe("STACK_METRICS", () => {
  it("contains no total, so a stack never draws the same bytes twice", () => {
    for (const m of STACK_METRICS) {
      expect(["footprint", "resident", "virtual"]).not.toContain(m);
    }
  });
});

describe("fmtCpuTime", () => {
  it("matches the CLI's units", () => {
    expect(fmtCpuTime(0)).toBe("0.0s");
    expect(fmtCpuTime(3.45)).toBe("3.5s");
    expect(fmtCpuTime(59.9)).toBe("59.9s");
    expect(fmtCpuTime(60)).toBe("1m00s");
    expect(fmtCpuTime(125)).toBe("2m05s");
    expect(fmtCpuTime(3600)).toBe("1h00m");
    expect(fmtCpuTime(7860)).toBe("2h11m");
  });
});

describe("fmtPercent", () => {
  it("keeps a decimal below 10% and rounds above", () => {
    // A dev server idling at 0.4% and one at 0% are different facts; rounding
    // both to "0%" loses the only signal there.
    expect(fmtPercent(0)).toBe("0.0%");
    expect(fmtPercent(0.42)).toBe("0.4%");
    expect(fmtPercent(9.96)).toBe("10.0%");
    expect(fmtPercent(37.4)).toBe("37%");
  });

  it("does not clamp above 100 — a multi-threaded tree really does exceed one core", () => {
    expect(fmtPercent(340)).toBe("340%");
  });
});

describe("stack presence (F1 regression)", () => {
  // Two processes; the second only exists for the last two of five buckets —
  // a child that restarted mid-window, which is routine for `npm run dev`.
  const nodeProc = [10, 10, 10, 10, 10];
  const lateChild = [null, null, null, 5, 5];

  it("all-or-nothing presence blanks the whole stack where any series is absent", () => {
    // This is the OLD behaviour and the bug: every bucket before the child
    // appeared is dropped for BOTH processes, so the chart goes empty even
    // though the parent has data throughout.
    const every = (i: number) => [nodeProc, lateChild].every((p) => p[i] != null);
    expect(contiguousRuns(5, every)).toEqual([[3, 4]]);
  });

  it("per-series presence keeps every bucket a live process reported", () => {
    // The fix: absence of a process means it did not exist, which contributes
    // zero — the sum of the live processes still IS the tree's figure.
    const some = (i: number) => [nodeProc, lateChild].some((p) => p[i] != null);
    expect(contiguousRuns(5, some)).toEqual([[0, 1, 2, 3, 4]]);
  });

  it("page classes still need all-or-nothing", () => {
    // An unmeasurable class is not zero, so a partial stack would understate
    // the total — the opposite policy from processes, deliberately.
    const priv = [1, 1, null, 1, 1];
    const shared = [2, 2, 2, 2, 2];
    const every = (i: number) => [priv, shared].every((p) => p[i] != null);
    expect(contiguousRuns(5, every)).toEqual([
      [0, 1],
      [3, 4],
    ]);
  });
});

describe("stackBands", () => {
  // Identity scales so the assertions read as values, not pixels.
  const x = (i: number) => i;
  const y = (v: number) => v;
  const series = (key: string, points: (number | null)[]): ChartSeries => ({
    key,
    label: key,
    slot: 1,
    points,
  });

  it("lays each band on the sum of the ones below it", () => {
    const { bands } = stackBands(
      [series("a", [1, 1, 1]), series("b", [2, 2, 2])],
      3,
      "all",
      x,
      y,
    );
    // `a` runs along the baseline: top edge at 1, bottom edge back at 0.
    expect(bands[0].d).toBe("M0.0,1.0 L1.0,1.0 L2.0,1.0 L2.0,0.0 L1.0,0.0 L0.0,0.0 Z");
    // `b` sits ON `a`: top edge at 3, bottom edge at 1 — not at 0.
    expect(bands[1].d).toBe("M0.0,3.0 L1.0,3.0 L2.0,3.0 L2.0,1.0 L1.0,1.0 L0.0,1.0 Z");
  });

  it('"any" keeps drawing where one band is absent, treating it as zero', () => {
    // The F1 case: `b` is a child that only existed at the last index.
    const { bands } = stackBands(
      [series("a", [1, 1, 1]), series("b", [null, null, 2])],
      3,
      "any",
      x,
      y,
    );
    // `a` spans all three indices — it is not blanked by `b`'s absence.
    expect(bands[0].d).toBe("M0.0,1.0 L1.0,1.0 L2.0,1.0 L2.0,0.0 L1.0,0.0 L0.0,0.0 Z");
    // `b` contributes 0 where absent, so its band hugs `a`'s top until index 2.
    expect(bands[1].d).toBe("M0.0,1.0 L1.0,1.0 L2.0,3.0 L2.0,1.0 L1.0,1.0 L0.0,1.0 Z");
  });

  it('"all" drops the index entirely where any band is absent', () => {
    const { bands } = stackBands(
      [series("a", [1, 1, 1]), series("b", [2, null, 2])],
      3,
      "all",
      x,
      y,
    );
    // Index 1 is gone, so each band is two single-point runs (drawn as ticks).
    expect(bands[0].d).toBe("M0.0,0.0 L0.0,1.0 M2.0,0.0 L2.0,1.0");
  });

  it("does not raise the baseline under a gap", () => {
    // The silent-failure mode: if the baseline accumulated at every index rather
    // than only the drawn ones, `b`'s run after the gap would start too high.
    const { bands } = stackBands(
      [series("a", [5, null, 5]), series("b", [1, 1, 1])],
      3,
      "all",
      x,
      y,
    );
    // Index 1 is dropped for both (policy "all"), and index 2's baseline is 5 —
    // `a`'s value there — not 10 (the sum of both of `a`'s values).
    expect(bands[1].d).toContain("2.0,6.0");
    expect(bands[1].d).not.toContain("2.0,11.0");
  });

  it("publishes tops that match the drawn paths, null where nothing is drawn", () => {
    // The crosshair dots used to recompute this sum themselves, and the two
    // disagreed exactly where a band was dropped: under "all" an index with any
    // absent series is in no path, yet the dots placed themselves on a baseline
    // the stack never had — dots floating over a hole. `tops` is now the single
    // source both read.
    const { tops } = stackBands(
      [series("a", [1, 1, 1]), series("b", [2, null, 2])],
      3,
      "all",
      x,
      y,
    );
    // Index 1 is dropped for the whole stack, so no band has a top there.
    expect(tops[0][1]).toBeNull();
    expect(tops[1][1]).toBeNull();
    // Where drawn, a band's top is the cumulative sum including itself.
    expect(tops[0][0]).toBe(1);
    expect(tops[1][0]).toBe(3);
    expect(tops[0][2]).toBe(1);
    expect(tops[1][2]).toBe(3);
  });

  it('under "any", a band absent at an index still has a top to sit on', () => {
    const { tops } = stackBands(
      [series("a", [1, 1, 1]), series("b", [null, null, 2])],
      3,
      "any",
      x,
      y,
    );
    // `b` contributes 0 where absent, so its top equals `a`'s there — the dot
    // sits on the stack, which is what the path draws.
    expect(tops[1][0]).toBe(1);
    expect(tops[1][2]).toBe(3);
  });

  it("stackedMax sums per index and ignores absences", () => {
    expect(stackedMax([series("a", [1, 4]), series("b", [2, null])], 2)).toBe(4);
  });
});

describe("assignSlots", () => {
  it("never gives two drawn keys the same slot", () => {
    // The first regression: `size % 8` collided as soon as more than eight keys
    // had been seen, putting two bands of the SAME colour in one chart at once.
    const keys = Array.from({ length: PALETTE_SLOTS }, (_, i) => `${1000 + i}`);
    const got = assignSlots(keys, new Map());
    expect(got.size).toBe(PALETTE_SLOTS);
    expect(new Set(got.values()).size).toBe(PALETTE_SLOTS);
  });

  it("keeps a key's colour across polls", () => {
    const first = assignSlots(["1", "2", "3"], new Map());
    // The server reorders (it ranks by peak, which moves) — colours must not.
    const second = assignSlots(["3", "1", "2"], first);
    for (const k of ["1", "2", "3"]) expect(second.get(k)).toBe(first.get(k));
  });

  it("reuses a slot only after its key stops being drawn", () => {
    const first = assignSlots(["1", "2"], new Map());
    const freed = first.get("1")!;
    const second = assignSlots(["2", "9"], first);
    expect(second.get("2")).toBe(first.get("2"));
    expect(second.get("9")).toBe(freed);
    expect(new Set(second.values()).size).toBe(2);
  });

  it("survives a stale previous map that would collide", () => {
    const stale = new Map([
      ["1", 1],
      ["2", 1],
    ]);
    const got = assignSlots(["1", "2"], stale);
    expect(new Set(got.values()).size).toBe(2);
  });

  it("ignores an out-of-range previous slot", () => {
    const got = assignSlots(["1"], new Map([["1", 99]]));
    expect(got.get("1")).toBeGreaterThanOrEqual(1);
    expect(got.get("1")).toBeLessThanOrEqual(PALETTE_SLOTS);
  });

  it('gives "Other" a distinct hue at the 8→9 process transition', () => {
    // The THIRD regression in this code, and why "Other" is allocated in the same
    // pass instead of being pinned to a reserved slot: with 8 processes one of
    // them holds slot 8; when a 9th appears the panel folds to 7 individuals plus
    // "Other", and a reserved slot 8 collided with whichever individual still
    // carried it.
    const eight = Array.from({ length: 8 }, (_, i) => `${100 + i}`);
    const poll1 = assignSlots(eight, new Map());
    expect(new Set(poll1.values()).size).toBe(8);

    // Fold: the first 7 individuals survive, the rest become one "Other" band.
    const folded = [...eight.slice(0, 7), "other"];
    const poll2 = assignSlots(folded, poll1);
    expect(poll2.size).toBe(8);
    expect(new Set(poll2.values()).size).toBe(8);
    // Specifically: nothing shares a hue with "Other".
    const otherSlot = poll2.get("other")!;
    for (const k of eight.slice(0, 7)) {
      expect(poll2.get(k)).not.toBe(otherSlot);
    }
    // And the survivors kept their colours.
    for (const k of eight.slice(0, 7)) {
      expect(poll2.get(k)).toBe(poll1.get(k));
    }
  });

  it("handles an empty key list", () => {
    expect(assignSlots([], new Map()).size).toBe(0);
  });
});
