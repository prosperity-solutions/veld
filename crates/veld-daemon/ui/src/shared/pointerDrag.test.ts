import { describe, expect, it } from "vitest";

import {
  AUTOSCROLL_BAND,
  AUTOSCROLL_MAX,
  autoScrollVelocity,
  beyondThreshold,
  seeThrough,
} from "./pointerDrag";

describe("beyondThreshold", () => {
  it("lets a press wobble without becoming a drag", () => {
    // A rail row is both selected and moved by the same press, so a hand that
    // shifts a pixel between down and up has to still count as a click.
    expect(beyondThreshold(0, 0)).toBe(false);
    expect(beyondThreshold(3, 0)).toBe(false);
    expect(beyondThreshold(0, -3)).toBe(false);
  });

  it("counts travel, not either axis on its own", () => {
    // 3px right and 3px down is 4.2px of movement. Testing the axes separately
    // would call this a click on both counts and no diagonal drag would start.
    expect(beyondThreshold(3, 3)).toBe(true);
    expect(beyondThreshold(-3, 3)).toBe(true);
  });

  it("starts at the threshold rather than past it", () => {
    expect(beyondThreshold(4, 0)).toBe(true);
    expect(beyondThreshold(0, 4)).toBe(true);
  });
});

describe("autoScrollVelocity", () => {
  // A container taller than two bands, so the two do not overlap.
  const top = 100;
  const bottom = 500;

  it("leaves the middle alone", () => {
    expect(autoScrollVelocity(top, bottom, 300)).toBe(0);
    expect(autoScrollVelocity(top, bottom, top + AUTOSCROLL_BAND)).toBe(0);
    expect(autoScrollVelocity(top, bottom, bottom - AUTOSCROLL_BAND)).toBe(0);
  });

  it("scrolls up near the top and down near the bottom", () => {
    expect(autoScrollVelocity(top, bottom, top + 1)).toBeLessThan(0);
    expect(autoScrollVelocity(top, bottom, bottom - 1)).toBeGreaterThan(0);
  });

  it("has no dead rim at the edge of the band", () => {
    // One pixel inside the band rounds to zero without the floor, so the moment
    // a drag enters the band would be the moment autoscroll looks broken.
    expect(autoScrollVelocity(top, bottom, top + AUTOSCROLL_BAND - 1)).toBe(-1);
    expect(autoScrollVelocity(top, bottom, bottom - AUTOSCROLL_BAND + 1)).toBe(1);
  });

  it("ramps with depth and tops out at the edge", () => {
    const shallow = autoScrollVelocity(top, bottom, top + 20);
    const deep = autoScrollVelocity(top, bottom, top + 4);
    expect(Math.abs(deep)).toBeGreaterThan(Math.abs(shallow));
    expect(autoScrollVelocity(top, bottom, top)).toBe(-AUTOSCROLL_MAX);
    expect(autoScrollVelocity(top, bottom, bottom)).toBe(AUTOSCROLL_MAX);
  });

  it("clamps past the container instead of accelerating forever", () => {
    // A pointer far above the list is still "scroll up", not "scroll up 400px
    // a frame" — the speed it gets is the one the edge already gives.
    expect(autoScrollVelocity(top, bottom, top - 300)).toBe(-AUTOSCROLL_MAX);
    expect(autoScrollVelocity(top, bottom, bottom + 300)).toBe(AUTOSCROLL_MAX);
  });

  it("gives a short container both directions", () => {
    // Shorter than two bands, so every point sits in both. The deeper side
    // wins; taking the first match would make a short list only ever scroll up.
    const shortBottom = AUTOSCROLL_BAND;
    expect(autoScrollVelocity(0, shortBottom, 2)).toBeLessThan(0);
    expect(autoScrollVelocity(0, shortBottom, AUTOSCROLL_BAND - 2)).toBeGreaterThan(0);
  });
});

describe("seeThrough", () => {
  // What decides whether the flying copy of a row gets handed the rail's
  // surface to carry. Get it wrong in the permissive direction and every ghost
  // is a card that should have been the square's own colour; get it wrong the
  // other way and a row dragged over a terminal is dark text on a dark page.
  it("treats a fully transparent background as see-through", () => {
    // What Chromium answers for `background: transparent`, which is what
    // `.wt-row` and `.lane-head` both declare.
    expect(seeThrough("rgba(0, 0, 0, 0)")).toBe(true);
    expect(seeThrough("transparent")).toBe(true);
  });

  it("treats an unset background as see-through", () => {
    // `surfaceUnder` walks past `<html>` and returns "", and an element with no
    // computed value at all is likewise nothing to see the page through.
    expect(seeThrough("")).toBe(true);
  });

  it("does not read an opaque colour's blue channel as an alpha", () => {
    // The three-argument form has no alpha, and a regex loose enough to match
    // it would take 247 for one — which reads every opaque light colour as
    // see-through and backs the project squares for no reason.
    expect(seeThrough("rgb(245, 246, 247)")).toBe(false);
    expect(seeThrough("rgb(0, 0, 0)")).toBe(false);
  });

  it("backs a partly transparent colour too", () => {
    // A half-alpha row over a terminal is still a row you cannot read.
    expect(seeThrough("rgba(255, 255, 255, 0.5)")).toBe(true);
    expect(seeThrough("rgba(255, 255, 255, 1)")).toBe(false);
  });

  it("reads the alpha out of the modern colour forms too", () => {
    // Not hypothetical: `.rail-group.drop-in` is a `color-mix(in oklab, …)`
    // and Chromium hands that back as `oklab(L a b / A)`, not as `rgba()`.
    // The first two are the exact strings measured off the running app.
    expect(seeThrough("oklab(0.186986 -0.00893791 -0.0236999 / 0.09)")).toBe(
      true,
    );
    expect(
      seeThrough("color(srgb 0.0392157 0.0784314 0.117647 / 0.88)"),
    ).toBe(true);
    expect(seeThrough("hsl(210 40% 96% / 50%)")).toBe(true);
  });

  it("does not invent an alpha for an opaque modern colour", () => {
    // No slash, no alpha. The channels must not be mistaken for one — the
    // whole point of anchoring the match to the closing paren.
    expect(seeThrough("oklab(0.186986 -0.00893791 -0.0236999)")).toBe(false);
    expect(seeThrough("color(srgb 0.0392157 0.0784314 0.117647)")).toBe(false);
    expect(seeThrough("oklch(0.7 0.1 240 / 1)")).toBe(false);
    expect(seeThrough("hsl(210 40% 96% / 100%)")).toBe(false);
  });
});
