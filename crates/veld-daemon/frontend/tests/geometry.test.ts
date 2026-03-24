import { describe, it, expect } from "vitest";
import { dist, pathLength, computeBBox, constrainToAxis } from "../src/draw-overlay/geometry";

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

describe("constrainToAxis", () => {
  it("snaps to horizontal when dx > dy", () => {
    const result = constrainToAxis(p(100, 100), p(250, 110));
    expect(result.x).toBe(250);
    expect(result.y).toBe(100);
  });

  it("snaps to vertical when dy > dx", () => {
    const result = constrainToAxis(p(100, 100), p(110, 250));
    expect(result.x).toBe(100);
    expect(result.y).toBe(250);
  });

  it("snaps to horizontal when dx == dy (tie goes horizontal)", () => {
    const result = constrainToAxis(p(0, 0), p(50, 50));
    expect(result.x).toBe(50);
    expect(result.y).toBe(0);
  });

  it("works with negative directions (left)", () => {
    const result = constrainToAxis(p(200, 100), p(50, 110));
    expect(result.x).toBe(50);
    expect(result.y).toBe(100);
  });

  it("works with negative directions (up)", () => {
    const result = constrainToAxis(p(100, 200), p(110, 50));
    expect(result.x).toBe(100);
    expect(result.y).toBe(50);
  });

  it("preserves pressure from cursor", () => {
    const anchor = { x: 0, y: 0, pressure: 0.5 };
    const cursor = { x: 100, y: 10, pressure: 0.8 };
    const result = constrainToAxis(anchor, cursor);
    expect(result.pressure).toBe(0.8);
  });

  it("returns anchor position when cursor is at same point", () => {
    const result = constrainToAxis(p(100, 100), p(100, 100));
    expect(result.x).toBe(100);
    expect(result.y).toBe(100);
  });
});
