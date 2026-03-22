import { describe, it, expect } from "vitest";
import { computeScrubValue } from "../src/shared/number-scrub";

describe("computeScrubValue", () => {
  it("increases value when dragging right", () => {
    expect(computeScrubValue(100, 10, 1, { min: 0, max: 1000, step: 1 })).toBe(110);
  });

  it("decreases value when dragging left", () => {
    expect(computeScrubValue(100, -20, 1, { min: 0, max: 1000, step: 1 })).toBe(80);
  });

  it("respects step size", () => {
    expect(computeScrubValue(100, 15, 1, { min: 0, max: 1000, step: 10 })).toBe(250);
  });

  it("clamps to min", () => {
    expect(computeScrubValue(10, -100, 1, { min: 0, max: 1000, step: 1 })).toBe(0);
  });

  it("clamps to max", () => {
    expect(computeScrubValue(990, 100, 1, { min: 0, max: 1000, step: 1 })).toBe(1000);
  });

  it("shift multiplier = 10x speed", () => {
    expect(computeScrubValue(100, 5, 10, { min: 0, max: 1000, step: 1 })).toBe(150);
  });

  it("ctrl multiplier = 0.1x precision", () => {
    expect(computeScrubValue(100, 10, 0.1, { min: 0, max: 1000, step: 1 })).toBe(101);
  });

  it("works with floating point step", () => {
    const result = computeScrubValue(1.0, 5, 1, { min: 0, max: 10, step: 0.1 });
    expect(result).toBeCloseTo(1.5);
  });

  it("no min/max means unbounded", () => {
    expect(computeScrubValue(0, -100, 1, { step: 1 })).toBe(-100);
    expect(computeScrubValue(0, 100, 1, { step: 1 })).toBe(100);
  });
});
