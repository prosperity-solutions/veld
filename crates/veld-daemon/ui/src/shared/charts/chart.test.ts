import { describe, expect, it } from "vitest";

import { axisMax, contiguousRuns } from "./TimeSeriesChart";
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
    expect(fmtCpuTime(3.14)).toBe("3.1s");
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
