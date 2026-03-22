import { describe, it, expect } from "vitest";
import { hitTest } from "../src/draw-overlay/hit-test";
import type { StrokeEntry, PinEntry, StrokeDraw, BlurEntry, SpotlightEntry, Point } from "../src/draw-overlay/types";

const p = (x: number, y: number): Point => ({ x, y, pressure: 0.5 });

function makeStroke(points: Point[]): StrokeDraw {
  return {
    points,
    color: "#ef4444",
    baseWidth: 5,
    compositeOp: "source-over",
    hasPressure: false,
    toolMode: "draw",
  };
}

function makePin(x: number, y: number, num: number): PinEntry {
  return { type: "pin", x, y, number: num, color: "#ef4444", angle: 0 };
}

function makeBlur(x: number, y: number, w: number, h: number): BlurEntry {
  return { type: "blur", bbox: { x, y, w, h }, pixelCanvas: null as unknown as HTMLCanvasElement };
}

function makeSpotlight(points: Point[]): SpotlightEntry {
  return { type: "spotlight", points, shape: null };
}

describe("hitTest", () => {
  it("returns null for empty strokes", () => {
    expect(hitTest([], p(100, 100), 10)).toBeNull();
  });

  it("hits a pin within radius", () => {
    const strokes: StrokeEntry[] = [makePin(100, 100, 1)];
    // Click right on the pin center
    expect(hitTest(strokes, p(100, 100), 10)).toBe(0);
    // Click within radius (16px)
    expect(hitTest(strokes, p(110, 100), 10)).toBe(0);
  });

  it("misses a pin outside radius", () => {
    const strokes: StrokeEntry[] = [makePin(100, 100, 1)];
    // Click far away
    expect(hitTest(strokes, p(200, 200), 10)).toBeNull();
  });

  it("hits a freehand stroke near the path", () => {
    const stroke = makeStroke([p(0, 0), p(100, 0), p(200, 0)]);
    const strokes: StrokeEntry[] = [stroke];
    // Click 3px below the horizontal line — within threshold
    expect(hitTest(strokes, p(50, 3), 10)).toBe(0);
  });

  it("misses a freehand stroke far from path", () => {
    const stroke = makeStroke([p(0, 0), p(100, 0), p(200, 0)]);
    const strokes: StrokeEntry[] = [stroke];
    // Click 50px below — too far
    expect(hitTest(strokes, p(50, 50), 10)).toBeNull();
  });

  it("hits blur entry inside bbox", () => {
    const strokes: StrokeEntry[] = [makeBlur(50, 50, 100, 80)];
    expect(hitTest(strokes, p(75, 75), 10)).toBe(0);
  });

  it("misses blur entry outside bbox", () => {
    const strokes: StrokeEntry[] = [makeBlur(50, 50, 100, 80)];
    expect(hitTest(strokes, p(200, 200), 10)).toBeNull();
  });

  it("hits spotlight inside bounding box", () => {
    const strokes: StrokeEntry[] = [
      makeSpotlight([p(10, 10), p(110, 10), p(110, 110), p(10, 110)]),
    ];
    expect(hitTest(strokes, p(60, 60), 10)).toBe(0);
  });

  it("returns top-most stroke (last in array) when overlapping", () => {
    const strokes: StrokeEntry[] = [
      makePin(100, 100, 1), // index 0
      makePin(100, 100, 2), // index 1 — on top
    ];
    // Both pins overlap — should return index 1 (top-most)
    expect(hitTest(strokes, p(100, 100), 10)).toBe(1);
  });

  it("handles single-point stroke", () => {
    const stroke = makeStroke([p(50, 50)]);
    const strokes: StrokeEntry[] = [stroke];
    // Single point — no segments to check, use point distance
    expect(hitTest(strokes, p(52, 52), 10)).toBe(0);
    expect(hitTest(strokes, p(100, 100), 10)).toBeNull();
  });

  it("hits shape stroke via bounding box", () => {
    const stroke = makeStroke([p(0, 0), p(100, 100)]);
    stroke.shape = { type: "line", start: p(0, 0), end: p(100, 100) };
    const strokes: StrokeEntry[] = [stroke];
    // Click near the line
    expect(hitTest(strokes, p(50, 50), 10)).toBe(0);
  });
});
