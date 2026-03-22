import { describe, it, expect } from "vitest";
import { autoColor } from "../src/draw-overlay/color";

describe("autoColor", () => {
  it("returns red for light backgrounds (luminance > 128)", () => {
    expect(autoColor(200)).toBe("#ef4444");
    expect(autoColor(255)).toBe("#ef4444");
    expect(autoColor(129)).toBe("#ef4444");
  });

  it("returns green for dark backgrounds (luminance <= 128)", () => {
    expect(autoColor(0)).toBe("#C4F56A");
    expect(autoColor(50)).toBe("#C4F56A");
    expect(autoColor(128)).toBe("#C4F56A");
  });

  it("returns red as fallback for negative luminance (no snapshot)", () => {
    expect(autoColor(-1)).toBe("#ef4444");
  });
});
