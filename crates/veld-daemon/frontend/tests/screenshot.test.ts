// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { computeContainRect } from "../src/feedback-overlay/screenshot";

describe("computeContainRect", () => {
  it("fills the box exactly when the aspect ratio matches", () => {
    expect(computeContainRect(1920, 1080, 1920, 1080)).toEqual({ x: 0, y: 0, w: 1920, h: 1080 });
  });

  it("letterboxes top/bottom when the bitmap is wider than the box", () => {
    // 2:1 bitmap into a 1:1 box — this is the exact shape of the original bug:
    // a captured frame whose aspect ratio doesn't match the viewport used to
    // get stretched via background-size:100% 100% instead of fitted like this.
    const rect = computeContainRect(2000, 1000, 1000, 1000);
    expect(rect).toEqual({ x: 0, y: 250, w: 1000, h: 500 });
  });

  it("letterboxes left/right when the bitmap is taller than the box", () => {
    const rect = computeContainRect(1000, 2000, 1000, 1000);
    expect(rect).toEqual({ x: 250, y: 0, w: 500, h: 1000 });
  });

  it("scales down uniformly when the bitmap is larger than the box on both axes", () => {
    const rect = computeContainRect(3840, 2160, 1920, 1080);
    expect(rect).toEqual({ x: 0, y: 0, w: 1920, h: 1080 });
  });
});
