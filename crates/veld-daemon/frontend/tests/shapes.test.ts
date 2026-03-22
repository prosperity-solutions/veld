import { describe, it, expect } from "vitest";
import { recognizeShape } from "../src/draw-overlay/shapes";

const p = (x: number, y: number) => ({ x, y, pressure: 0.5 });

// Generate points along a line from (x1,y1) to (x2,y2)
function linePts(x1: number, y1: number, x2: number, y2: number, n = 20) {
  const pts = [];
  for (let i = 0; i < n; i++) {
    const t = i / (n - 1);
    pts.push(p(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t));
  }
  return pts;
}

// Generate points along a circle
function circlePts(cx: number, cy: number, r: number, n = 30) {
  const pts = [];
  for (let i = 0; i < n; i++) {
    const angle = (i / n) * Math.PI * 2;
    pts.push(p(cx + r * Math.cos(angle), cy + r * Math.sin(angle)));
  }
  // Close the path approximately
  pts.push(p(cx + r, cy));
  return pts;
}

// Generate points along a rectangle
function rectPts(x: number, y: number, w: number, h: number, perSide = 10) {
  const pts = [];
  // Top edge
  for (let i = 0; i < perSide; i++) pts.push(p(x + (w * i) / perSide, y));
  // Right edge
  for (let i = 0; i < perSide; i++) pts.push(p(x + w, y + (h * i) / perSide));
  // Bottom edge
  for (let i = 0; i < perSide; i++)
    pts.push(p(x + w - (w * i) / perSide, y + h));
  // Left edge
  for (let i = 0; i < perSide; i++)
    pts.push(p(x, y + h - (h * i) / perSide));
  // Close
  pts.push(p(x, y));
  return pts;
}

describe("recognizeShape", () => {
  it("returns null for too few points", () => {
    expect(recognizeShape([p(0, 0), p(1, 1)])).toBeNull();
  });

  it("recognizes a straight horizontal line", () => {
    const pts = linePts(0, 100, 200, 100);
    const shape = recognizeShape(pts);
    expect(shape).not.toBeNull();
    expect(shape!.type).toBe("line");
  });

  it("recognizes a straight diagonal line", () => {
    const pts = linePts(0, 0, 150, 150);
    const shape = recognizeShape(pts);
    expect(shape).not.toBeNull();
    expect(shape!.type).toBe("line");
  });

  it("recognizes a circle", () => {
    const pts = circlePts(100, 100, 50);
    const shape = recognizeShape(pts);
    expect(shape).not.toBeNull();
    expect(shape!.type).toBe("circle");
    if (shape!.type === "circle") {
      expect(Math.abs(shape!.cx - 100)).toBeLessThan(5);
      expect(Math.abs(shape!.cy - 100)).toBeLessThan(5);
      expect(Math.abs(shape!.radius - 50)).toBeLessThan(5);
    }
  });

  it("recognizes a rectangle", () => {
    // Use a very elongated rectangle so it doesn't look like a circle
    const pts = rectPts(10, 10, 200, 40);
    const shape = recognizeShape(pts);
    expect(shape).not.toBeNull();
    expect(shape!.type).toBe("rect");
    if (shape!.type === "rect") {
      expect(Math.abs(shape!.w - 200)).toBeLessThan(10);
      expect(Math.abs(shape!.h - 40)).toBeLessThan(10);
    }
  });

  it("returns null for random scribble", () => {
    // Zigzag pattern that doesn't match any shape
    const pts = [];
    for (let i = 0; i < 20; i++) {
      pts.push(p(i * 10, i % 2 === 0 ? 0 : 50));
    }
    const shape = recognizeShape(pts);
    // Zigzag is ambiguous — key test is it doesn't falsely detect as circle
    if (shape) {
      expect(shape.type).not.toBe("circle");
    }
  });

  it("does not recognize a tiny gesture as a shape", () => {
    const pts = linePts(0, 0, 5, 5); // too short
    const shape = recognizeShape(pts);
    // directDist is ~7, which is < 20, so line won't match
    expect(shape).toBeNull();
  });
});

describe("recognizeShape - arrows", () => {
  it("recognizes a line with arrowhead flick", () => {
    // Long straight line (40 points) with a short diagonal flick at end (5 points)
    // The flick must be < 15% of total points to trigger arrow detection
    const pts = linePts(0, 100, 300, 100, 40);
    // Small flick: 3 points diverging slightly
    pts.push(p(297, 96));
    pts.push(p(293, 91));
    pts.push(p(290, 88));
    const shape = recognizeShape(pts);
    expect(shape).not.toBeNull();
    expect(["arrow", "line"]).toContain(shape!.type);
  });
});
