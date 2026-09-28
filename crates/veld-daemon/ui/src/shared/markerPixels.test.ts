import { describe, expect, it } from "vitest";

import { cellLit, isBrightColor, litCells, PIXEL_PATTERNS, pixelPattern } from "./markerPixels";

/** Whether a mask reads the same with its left and right columns swapped. */
function mirrored(mask: number): boolean {
  return [0, 1, 2].every((row) => cellLit(mask, row, 0) === cellLit(mask, row, 2));
}

describe("PIXEL_PATTERNS", () => {
  it("has one distinct, 9-bit pattern per slot", () => {
    // Distinctness is the whole claim: two glyphs sharing a pattern would put two
    // checkouts of one repo behind the same marker.
    expect(new Set(PIXEL_PATTERNS).size).toBe(PIXEL_PATTERNS.length);
    for (const m of PIXEL_PATTERNS) expect(m).toBeGreaterThanOrEqual(0);
    for (const m of PIXEL_PATTERNS) expect(m).toBeLessThan(512);
  });

  it("keeps its first 64 slots exactly as shipped", () => {
    // Slot i is what glyph i looks like, so moving or replacing one repaints every
    // checkout wearing it. Append new slots after these; never edit this string.
    expect(PIXEL_PATTERNS.slice(0, 64).join(",")).toBe(
      "273,5,40,45,320,325,360,365,84,7,42,47,322,327,362,367,79,21,56,61,336,341,376,381,18,23,58,63,338,343,378,383,295,133,168,173,448,453,488,493,130,135,170,175,450,455,490,495,144,149,184,189,464,469,504,509,146,151,186,191,466,471,506,511",
    );
  });

  it("covers the 64 animals", () => {
    // The Rust side checks the same thing against `WORKTREE_EMOJI` itself
    // (`every_curated_emoji_has_a_pixel_pattern`); this pins the table's own size.
    expect(PIXEL_PATTERNS.length).toBeGreaterThanOrEqual(64);
  });

  it("never draws a pattern too faint to tell apart", () => {
    for (const m of PIXEL_PATTERNS) expect(litCells(m)).toBeGreaterThanOrEqual(2);
  });

  it("is mirrored except for the four slots the faint patterns gave up", () => {
    const asymmetric = PIXEL_PATTERNS.filter((m) => !mirrored(m));
    expect(asymmetric).toHaveLength(4);
  });
});

describe("pixelPattern", () => {
  const order = ["🦊", "🐻", "🐼"];

  it("uses the glyph's slot in the daemon's order", () => {
    expect(pixelPattern("🦊", order)).toBe(PIXEL_PATTERNS[0]);
    expect(pixelPattern("🐼", order)).toBe(PIXEL_PATTERNS[2]);
  });

  it("gives a glyph outside the list a stable, reasonably lit pattern", () => {
    const a = pixelPattern("🦩", order);
    expect(pixelPattern("🦩", order)).toBe(a);
    expect(litCells(a)).toBeGreaterThanOrEqual(3);
  });
});

describe("isBrightColor", () => {
  it("splits the palette into its pale and its deep halves", () => {
    for (const c of ["#fff827", "#41fffc", "#7dff1a", "#ffa31a"]) expect(isBrightColor(c)).toBe(true);
    for (const c of ["#008cff", "#9719ff", "#ff17e0", "#ff3502"]) expect(isBrightColor(c)).toBe(false);
  });

  it("treats anything that is not a stored colour as deep", () => {
    expect(isBrightColor("var(--muted)")).toBe(false);
    expect(isBrightColor("")).toBe(false);
  });
});
