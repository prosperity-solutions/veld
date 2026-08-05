import { describe, expect, it } from "vitest";

import { randomFree, randomMarker } from "./markerPick";

const holder = (id: number) => [{ id, alias: `wt${id}` }];
/** A stub draw, so "random" is a fixed choice in a test. */
const draw = (v: number) => () => v;

describe("randomFree", () => {
  it("draws from the options no sibling holds", () => {
    const options = ["a", "b", "c"];
    const used = { a: holder(1) };
    // The free pool is ["b", "c"], so the draw indexes into that, not into `options`.
    expect(randomFree(options, used, draw(0))).toBe("b");
    expect(randomFree(options, used, draw(0.99))).toBe("c");
  });

  it("treats an empty holder list as free", () => {
    // `/api/repos` never sends an empty array today, but a key with no holders means
    // free, and reading it as taken would drop a colour from the pool for no reason.
    expect(randomFree(["a", "b"], { a: [] }, draw(0))).toBe("a");
  });

  it("falls back to the whole list when the repo has used everything", () => {
    // Matches the daemon: a repo with more checkouts than colours still gets one, and
    // the picker marks the duplicate rather than refusing.
    const used = { a: holder(1), b: holder(2) };
    expect(randomFree(["a", "b"], used, draw(0.99))).toBe("b");
  });

  it("cannot index past the end, even on a badly behaved draw", () => {
    // `random()` is specified as `< 1`; a stub — or a hostile shim — need not be, and
    // `pool[length]` would be `undefined` reaching the wire as a marker.
    expect(randomFree(["a", "b"], {}, draw(1))).toBe("b");
    expect(randomFree(["a", "b"], {}, draw(42))).toBe("b");
  });

  it("returns empty for an empty option list", () => {
    expect(randomFree([], {}, draw(0))).toBe("");
  });
});

describe("randomMarker", () => {
  it("draws the two faces independently", () => {
    // One draw value, two different pools: the point is that the glyph index is not
    // the colour index, so a colour re-pick never implies a glyph.
    const marker = randomMarker(
      ["🦊", "🐻", "🐼"],
      ["#008cff", "#41fffc"],
      { "🦊": holder(1) },
      {},
      draw(0),
    );
    expect(marker).toEqual({ emoji: "🐻", color: "#008cff" });
  });

  it("spreads across the pool rather than always proposing the same entry", () => {
    // The defect this replaced: "first free" gave every new worktree in a repo the
    // same marker. Ten draws across the unit interval must not collapse to one value.
    const colors = ["c1", "c2", "c3", "c4"];
    const seen = new Set(
      Array.from({ length: 10 }, (_, i) =>
        randomMarker(null, colors, {}, {}, draw(i / 10)).color,
      ),
    );
    expect(seen.size).toBeGreaterThan(1);
  });

  it("stays empty while the lists are still loading", () => {
    expect(randomMarker(null, null, {}, {}, draw(0))).toEqual({ emoji: "", color: "" });
    expect(randomMarker(["🦊"], null, {}, {}, draw(0))).toEqual({
      emoji: "🦊",
      color: "",
    });
  });
});
