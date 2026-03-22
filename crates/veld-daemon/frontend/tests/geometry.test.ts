import { describe, it, expect } from "vitest";
import { dist, pathLength, computeBBox } from "../src/draw-overlay/geometry";

const p = (x: number, y: number) => ({ x, y, pressure: 0.5 });

describe("dist", () => {
  it("returns 0 for same point", () => {
    expect(dist(p(5, 5), p(5, 5))).toBe(0);
  });

  it("computes horizontal distance", () => {
    expect(dist(p(0, 0), p(3, 0))).toBe(3);
  });

  it("computes vertical distance", () => {
    expect(dist(p(0, 0), p(0, 4))).toBe(4);
  });

  it("computes diagonal distance (3-4-5 triangle)", () => {
    expect(dist(p(0, 0), p(3, 4))).toBe(5);
  });
});

describe("pathLength", () => {
  it("returns 0 for single point", () => {
    expect(pathLength([p(0, 0)])).toBe(0);
  });

  it("returns 0 for empty array", () => {
    expect(pathLength([])).toBe(0);
  });

  it("sums segment lengths", () => {
    const points = [p(0, 0), p(3, 0), p(3, 4)];
    expect(pathLength(points)).toBe(7); // 3 + 4
  });
});

describe("computeBBox", () => {
  it("computes bounding box", () => {
    const points = [p(10, 20), p(30, 5), p(15, 40)];
    const bbox = computeBBox(points);
    expect(bbox.x).toBe(10);
    expect(bbox.y).toBe(5);
    expect(bbox.w).toBe(20); // 30 - 10
    expect(bbox.h).toBe(35); // 40 - 5
  });

  it("handles single point", () => {
    const bbox = computeBBox([p(5, 10)]);
    expect(bbox.x).toBe(5);
    expect(bbox.y).toBe(10);
    expect(bbox.w).toBe(0);
    expect(bbox.h).toBe(0);
  });
});
