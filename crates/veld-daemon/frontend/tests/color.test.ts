// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { autoColor, buildSnapshotCanvas, sampleLuminance } from "../src/draw-overlay/color";

describe("autoColor", () => {
  it("returns red for light backgrounds (luminance > 128)", () => {
    expect(autoColor(200)).toBe("#ef4444");
    expect(autoColor(129)).toBe("#ef4444");
  });

  it("returns green for dark backgrounds (luminance <= 128)", () => {
    expect(autoColor(0)).toBe("#C4F56A");
    expect(autoColor(128)).toBe("#C4F56A");
  });

  it("returns red as fallback for negative luminance (no snapshot)", () => {
    expect(autoColor(-1)).toBe("#ef4444");
  });

  it("threshold is exactly at 128", () => {
    expect(autoColor(128)).toBe("#C4F56A"); // dark → green
    expect(autoColor(129)).toBe("#ef4444"); // light → red
  });
});

describe("buildSnapshotCanvas", () => {
  it("returns null for null source", () => {
    expect(buildSnapshotCanvas(null)).toBeNull();
  });

  // Canvas getContext("2d") not implemented in jsdom — these need a real browser
  // Tested via E2E instead
});

describe("sampleLuminance", () => {
  it("returns -1 for null canvas", () => {
    expect(sampleLuminance(null, 10, 10)).toBe(-1);
  });

  // Canvas pixel sampling not available in jsdom — tested via E2E
});
