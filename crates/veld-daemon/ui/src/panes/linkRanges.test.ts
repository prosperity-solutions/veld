import { describe, expect, it } from "vitest";
import { cellOf, logicalBlockAt, type WrappedRow } from "./linkRanges";

/** A buffer of fixed-width rows, padded the way `translateToString(false)` pads. */
const buffer = (cols: number, rows: [string, boolean][]) => {
  const padded: WrappedRow[] = rows.map(([text, isWrapped]) => ({
    text: text.padEnd(cols, " "),
    isWrapped,
  }));
  return (y: number) => padded[y];
};

describe("logicalBlockAt", () => {
  it("returns an unwrapped row on its own", () => {
    const get = buffer(10, [["hello", false]]);
    expect(logicalBlockAt(get, 0, 10)).toEqual({ text: "hello     ", startY: 0 });
  });

  it("joins a wrapped block, whichever of its rows is asked about", () => {
    const get = buffer(5, [
      ["aaaaa", false],
      ["bbbbb", true],
      ["ccccc", true],
    ]);
    for (const asked of [0, 1, 2]) {
      expect(logicalBlockAt(get, asked, 5)).toEqual({
        text: "aaaaabbbbbccccc",
        startY: 0,
      });
    }
  });

  it("stops at the next unwrapped row rather than running on", () => {
    const get = buffer(5, [
      ["aaaaa", false],
      ["bbbbb", true],
      ["ccccc", false],
    ]);
    expect(logicalBlockAt(get, 0, 5)?.text).toBe("aaaaabbbbb");
    expect(logicalBlockAt(get, 2, 5)).toEqual({ text: "ccccc", startY: 2 });
  });

  it("walks back from a continuation row to the row that began it", () => {
    const get = buffer(4, [
      ["zzzz", false],
      ["aaaa", false],
      ["bbbb", true],
    ]);
    expect(logicalBlockAt(get, 2, 4)).toEqual({ text: "aaaabbbb", startY: 1 });
  });

  it("returns null past the end of the buffer", () => {
    expect(logicalBlockAt(buffer(4, [["aaaa", false]]), 9, 4)).toBeNull();
  });

  // The guard that exists instead of reading xterm's private `outColumns`.
  describe("rows whose width cannot be trusted", () => {
    it("refuses a block holding a full-width character", () => {
      // One CJK char is two cells but one string character, so the row comes back
      // one shorter than `cols`. Linking it would put every later underline one
      // cell left of where the text is.
      const get = (y: number) => [{ text: "日本 src".padEnd(9, " "), isWrapped: false }][y];
      expect(logicalBlockAt(get, 0, 10)).toBeNull();
    });

    it("refuses when the bad row is elsewhere in the same block", () => {
      const get = (y: number) =>
        [
          { text: "aaaaa", isWrapped: false },
          { text: "日本", isWrapped: true },
          { text: "ccccc", isWrapped: true },
        ][y];
      expect(logicalBlockAt(get, 2, 5)).toBeNull();
    });

    // The cap is a count, and both loops have to agree on that — they did not, and
    // each admitted one row more than the constant says. Pinned at the boundary
    // because that is the only place an off-by-one is visible.
    it.each([
      ["exactly the cap", 256, false],
      ["one past the cap", 257, true],
    ])("assembles a block of %s rows -> null: %s", (_what, rows, expectNull) => {
      const get = (y: number): WrappedRow | undefined =>
        y < rows ? { text: "ab", isWrapped: y > 0 } : undefined;
      // **Both ends, because there are two loops and they are capped separately.**
      // Asking only from the last row exercises the backward walk alone — the
      // forward bound could be reintroduced off by one and this stayed green.
      for (const asked of [rows - 1, 0]) {
        const block = logicalBlockAt(get, asked, 2);
        expect(block === null, `asked from row ${asked}`).toBe(expectNull);
        if (block) {
          expect(block.text.length).toBe(rows * 2);
        }
      }
    });

    it("accepts a row whose emoji happens to cancel out", () => {
      // A non-BMP emoji is two UTF-16 units and two cells, so the length still
      // matches and the arithmetic stays right.
      const get = (y: number) => [{ text: "🎉 ab", isWrapped: false }][y];
      expect(logicalBlockAt(get, 0, 5)?.text).toBe("🎉 ab");
    });
  });
});

describe("cellOf", () => {
  it("is 1-based in both axes", () => {
    expect(cellOf(0, 0, 80)).toEqual({ x: 1, y: 1 });
  });

  it("maps an offset inside the first row", () => {
    expect(cellOf(7, 0, 80)).toEqual({ x: 8, y: 1 });
  });

  it("wraps onto the next row at exactly `cols`", () => {
    expect(cellOf(79, 0, 80)).toEqual({ x: 80, y: 1 });
    expect(cellOf(80, 0, 80)).toEqual({ x: 1, y: 2 });
  });

  it("offsets by the block's own start row", () => {
    expect(cellOf(5, 12, 80)).toEqual({ x: 6, y: 13 });
    expect(cellOf(85, 12, 80)).toEqual({ x: 6, y: 14 });
  });

  it("round-trips a span the way the provider uses it", () => {
    // `end` is inclusive in an xterm range, so the last character is `end - 1`.
    const cols = 10;
    const start = cellOf(8, 3, cols);
    const end = cellOf(14 - 1, 3, cols);
    expect(start).toEqual({ x: 9, y: 4 });
    expect(end).toEqual({ x: 4, y: 5 });
  });
});
