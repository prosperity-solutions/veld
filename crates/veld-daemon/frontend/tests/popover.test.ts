// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { positionPopover } from "../src/feedback-overlay/popover";

// Mock an HTMLElement with style
function mockEl(): HTMLElement {
  return { style: {} } as any as HTMLElement;
}

describe("positionPopover", () => {
  it("positions below anchor by default", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 100, y: 50, width: 200, height: 30 });
    expect(parseFloat(pop.style.top!)).toBeGreaterThan(50); // below anchor
  });

  it("clamps left to viewport margin", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 0, y: 100, width: 10, height: 10 });
    const left = parseFloat(pop.style.left!);
    expect(left).toBeGreaterThanOrEqual(0); // not negative
  });

  it("sets top and left as pixel values", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 500, y: 200, width: 100, height: 50 });
    expect(pop.style.top).toMatch(/\d+px/);
    expect(pop.style.left).toMatch(/\d+px/);
  });
});
