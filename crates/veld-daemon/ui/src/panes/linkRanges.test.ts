import { describe, expect, it } from "vitest";
import { type Cell, type CellRow, hardWrappedBlockAt, logicalBlockAt, rangeOf, rangesOf } from "./linkRanges";
import { continuesPath, findFilePaths } from "./filePaths";

/**
 * A row of cells the way xterm reports them, from a string: one cell per
 * character, except `＊` stands for "a wide character" — the glyph in a width-2
 * cell followed by its width-0 placeholder — and `_` for an empty cell.
 */
function row(text: string, cols: number, isWrapped: boolean, wide = "日"): CellRow {
  const cells: Cell[] = [];
  for (const ch of text) {
    if (ch === "＊") {
      cells.push({ chars: wide, width: 2 }, { chars: "", width: 0 });
    } else if (ch === "_") {
      cells.push({ chars: "", width: 1 });
    } else {
      cells.push({ chars: ch, width: 1 });
    }
  }
  while (cells.length < cols) cells.push({ chars: "", width: 1 });
  return { cells, isWrapped };
}

const buffer =
  (rows: CellRow[]) =>
  (y: number): CellRow | undefined =>
    rows[y];

describe("logicalBlockAt", () => {
  it("returns an unwrapped row on its own, empty cells as spaces", () => {
    const block = logicalBlockAt(buffer([row("hello", 7, false)]), 0);
    expect(block?.text).toBe("hello  ");
    expect(block?.startY).toBe(0);
  });

  it("joins a wrapped block, whichever of its rows is asked about", () => {
    const get = buffer([row("aaaaa", 5, false), row("bbbbb", 5, true), row("cc", 5, true)]);
    for (const asked of [0, 1, 2]) {
      expect(logicalBlockAt(get, asked)?.text).toBe("aaaaabbbbbcc   ");
    }
  });

  it("stops at the next unwrapped row rather than running on", () => {
    const get = buffer([row("aaaaa", 5, false), row("bbbbb", 5, true), row("ccccc", 5, false)]);
    expect(logicalBlockAt(get, 0)?.text).toBe("aaaaabbbbb");
    expect(logicalBlockAt(get, 2)).toMatchObject({ text: "ccccc", startY: 2 });
  });

  it("walks back from a continuation row to the row that began it", () => {
    const get = buffer([row("zzzz", 4, false), row("aaaa", 4, false), row("bbbb", 4, true)]);
    expect(logicalBlockAt(get, 2)).toMatchObject({ text: "aaaabbbb", startY: 1 });
  });

  it("returns null past the end of the buffer", () => {
    expect(logicalBlockAt(buffer([row("aaaa", 4, false)]), 9)).toBeNull();
  });

  it("drops the empty cell a wide character leaves when it wraps", () => {
    // `src/a` fills four columns, the wide char does not fit in the fifth, so it
    // moves down and column five stays empty. On screen the line is unbroken.
    const get = buffer([row("src/_", 5, false), row("＊.md", 5, true)]);
    expect(logicalBlockAt(get, 1)?.text).toBe("src/日.md");
  });

  // The cap is a count, and both loops have to agree on that.
  it.each([
    ["exactly the cap", 256, false],
    ["one past the cap", 257, true],
  ])("assembles a block of %s rows -> null: %s", (_what, rows, expectNull) => {
    const get = (y: number): CellRow | undefined =>
      y < rows ? row("ab", 2, y > 0) : undefined;
    for (const asked of [rows - 1, 0]) {
      const block = logicalBlockAt(get, asked);
      expect(block === null, `asked from row ${asked}`).toBe(expectNull);
      if (block) expect(block.text.length).toBe(rows * 2);
    }
  });

  it("stops walking backwards instead of scanning the whole scrollback", () => {
    let reads = 0;
    const get = (y: number): CellRow | undefined => {
      reads += 1;
      return y >= 0 && y < 100_000 ? row("ab", 2, y > 0) : undefined;
    };
    expect(logicalBlockAt(get, 99_999)).toBeNull();
    expect(reads).toBeLessThan(1_000);
  });
});

describe("rangeOf", () => {
  it("is 1-based and inclusive", () => {
    const block = logicalBlockAt(buffer([row("ab src/x.ts", 12, false)]), 0);
    if (!block) throw new Error("no block");
    expect(rangeOf(block, 3, 11)).toEqual({ start: { x: 4, y: 1 }, end: { x: 11, y: 1 } });
  });

  it("spans every row of a wrapped path, from any row asked", () => {
    const get = buffer([
      row("see crates/ve", 13, false),
      row("ld-daemon/src", 13, true),
      row("/main.rs:44 x", 13, true),
    ]);
    for (const asked of [0, 1, 2]) {
      const block = logicalBlockAt(get, asked);
      if (!block) throw new Error("no block");
      const [match] = findFilePaths(block.text);
      expect(match.path).toBe("crates/veld-daemon/src/main.rs");
      expect(match.line).toBe(44);
      expect(rangeOf(block, match.start, match.end)).toEqual({
        start: { x: 5, y: 1 },
        end: { x: 11, y: 3 },
      });
    }
  });

  it("places a path after a full-width character on the right cells", () => {
    // The old arithmetic refused this whole line; one wide char before the path
    // shifts it right by one column, which the map follows.
    const block = logicalBlockAt(buffer([row("＊ docs/a.md", 12, false, "✅")]), 0);
    if (!block) throw new Error("no block");
    const [match] = findFilePaths(block.text);
    expect(match.path).toBe("docs/a.md");
    expect(rangeOf(block, match.start, match.end)).toEqual({
      start: { x: 4, y: 1 },
      end: { x: 12, y: 1 },
    });
  });

  it("ends on a wide character's second column", () => {
    const block = logicalBlockAt(buffer([row("a＊", 3, false)]), 0);
    if (!block) throw new Error("no block");
    expect(rangeOf(block, 0, 2)).toEqual({ start: { x: 1, y: 1 }, end: { x: 3, y: 1 } });
  });

  it("maps a non-BMP character's two units to its one cell", () => {
    const block = logicalBlockAt(buffer([row("＊ab", 4, false, "🎉")]), 0);
    if (!block) throw new Error("no block");
    expect(block.text).toBe("🎉ab");
    // "ab" starts at UTF-16 offset 2, on column 3.
    expect(rangeOf(block, 2, 4)).toEqual({ start: { x: 3, y: 1 }, end: { x: 4, y: 1 } });
  });

  it("is null for an empty or out-of-range span", () => {
    const block = logicalBlockAt(buffer([row("ab", 2, false)]), 0);
    if (!block) throw new Error("no block");
    expect(rangeOf(block, 1, 1)).toBeNull();
    expect(rangeOf(block, 0, 9)).toBeNull();
  });
});

describe("hardWrappedBlockAt", () => {
  // `continuesPath` from `filePaths.ts` is the real decision; these tests use it so
  // the join and the rule are pinned together, the way the terminal uses them.
  const rows = (lines: string[], cols = 50) => buffer(lines.map((l) => row(l, cols, false)));

  const CLAUDE = [
    "  File: /private/tmp/claude-501/-Users-peter-5d7",
    "  6a76f-1c2d/scratchpad/security-review",
    "  -plan.md",
  ];

  it("joins Claude Code's own wrapping of a long path, from any of its rows", () => {
    const get = rows(CLAUDE);
    for (const asked of [0, 1, 2]) {
      const block = hardWrappedBlockAt(get, asked, continuesPath);
      if (!block) throw new Error("no block");
      const [match] = findFilePaths(block.text);
      expect(match.path).toBe(
        "/private/tmp/claude-501/-Users-peter-5d76a76f-1c2d/scratchpad/security-review-plan.md",
      );
      // One range per printed piece, none of them covering the indentation.
      expect(rangesOf(block, match.start, match.end)).toEqual([
        { start: { x: 9, y: 1 }, end: { x: 48, y: 1 } },
        { start: { x: 3, y: 2 }, end: { x: 39, y: 2 } },
        { start: { x: 3, y: 3 }, end: { x: 10, y: 3 } },
      ]);
    }
  });

  it("does not join a finished path to the next line's first word", () => {
    const get = rows(["see src/a.ts", "  and more"]);
    const block = hardWrappedBlockAt(get, 0, continuesPath);
    expect(block?.text.trimEnd()).toBe("see src/a.ts");
    expect(findFilePaths(block?.text ?? "").map((m) => m.path)).toEqual(["src/a.ts"]);
  });

  it("does not join an absolute directory to ordinary prose", () => {
    const get = rows(["cd /usr/local", "  then run it"]);
    expect(hardWrappedBlockAt(get, 1, continuesPath)?.text.trim()).toBe("then run it");
  });

  it("does not join a line that merely ends in a word", () => {
    const get = rows(["Edited the file", "  src/a.ts"]);
    expect(hardWrappedBlockAt(get, 1, continuesPath)?.text.trim()).toBe("src/a.ts");
  });

  it("does not join a continuation with a space in it", () => {
    const get = rows(["open crates/veld-", "  daemon is fine"]);
    expect(hardWrappedBlockAt(get, 0, continuesPath)?.text.trim()).toBe("open crates/veld-");
  });

  it("does not join git status's untracked directory to the next untracked file", () => {
    const get = rows(["Untracked files:", "\tnotes/review/", "\tplan.md"]);
    expect(hardWrappedBlockAt(get, 1, continuesPath)?.text.trim()).toBe("notes/review/");
    expect(hardWrappedBlockAt(get, 2, continuesPath)?.text.trim()).toBe("plan.md");
  });

  it("joins a path broken right after a `/` when the rest is a path of its own", () => {
    const get = rows(["see crates/veld-daemon/", "  src/main.rs"]);
    const [match] = findFilePaths(hardWrappedBlockAt(get, 1, continuesPath)?.text ?? "");
    expect(match.path).toBe("crates/veld-daemon/src/main.rs");
  });

  it("keeps a :line tail on the piece that finishes the path", () => {
    const get = rows(["at crates/veld-daemon/sr", "  c/main.rs:44 here"]);
    const block = hardWrappedBlockAt(get, 1, continuesPath);
    const [match] = findFilePaths(block?.text ?? "");
    expect(match).toMatchObject({ path: "crates/veld-daemon/src/main.rs", line: 44 });
  });

  it("leaves soft-wrapped paths as one range across rows", () => {
    const get = buffer([row("x crates/ve", 11, false), row("ld/a.ts", 11, true)]);
    const block = hardWrappedBlockAt(get, 1, continuesPath);
    if (!block) throw new Error("no block");
    const [match] = findFilePaths(block.text);
    expect(rangesOf(block, match.start, match.end)).toEqual([
      { start: { x: 3, y: 1 }, end: { x: 7, y: 2 } },
    ]);
  });
});

describe("continuesPath", () => {
  it.each([
    ["/private/tmp/5d7", "6a76f-x/scratchpad/review", true],
    ["scratchpad/security-review", "-plan.md", true],
    ["src/a.ts", "and", false],
    ["/usr/local", "then", false],
    ["word", "src/a.ts", false],
    ["src/", "and", false],
    ["docs/pl", "an.md,", true],
    ["docs/pl", "an.md:12:3)", true],
    ["docs/pl", "an md", false],
    // `git status` untracked entries: a directory, then a sibling file. Two paths.
    ["notes/review/", "plan.md", false],
    ["/abs/dir/", "file.ts:3", false],
    // A break at a `/` still joins when the rest carries a `/` of its own.
    ["crates/veld-daemon/", "src/main.rs", true],
    ["/private/tmp/", "x/scratchpad/plan.md", true],
  ])("%s + %s -> %s", (tail, head, expected) => {
    expect(continuesPath(tail, head)).toBe(expected);
  });
});
