import { describe, expect, it } from "vitest";

import { parseDiffFromFile } from "@pierre/diffs";

import { parseTable, sortedOrder } from "./csv";
import { changeBlocks, diffRowOf, diffStops, newLineRange, rowOfLine } from "./deletions";
import { changeMarkersCss, changeStops } from "./gutter";
import { dispatchOpenFile, dispatchOpenUrl, onOpenFileRequest, onOpenUrlRequest } from "./openRequests";
import { formatReference, formatReferences, mergeRanges, referenceHeader, splitLines } from "./reference";
import { bundledLanguages } from "./shikiCurated";
import {
  BY_EXTENSION,
  CODE_EXTENSIONS,
  CODE_NAMES,
  LANGUAGES,
  type TextKind,
  curatedLanguages,
  languageFor,
  textKind,
  viewableAsText,
} from "./textKind";
// The fixture `veld_core::files` checks its lists against — see textKind.ts.
import fixture from "../../../../veld-core/src/text_kinds.json";

describe("copy reference", () => {
  const lines = splitLines("one\ntwo\n\nfour  \nfive");

  it("names a single line without a range", () => {
    expect(formatReference("docs/plan.md", lines, { start: 2, end: 2 })).toBe(
      "docs/plan.md:2\n> two",
    );
  });

  it("quotes every line of a range, a blank one as a bare >", () => {
    expect(formatReference("docs/plan.md", lines, { start: 2, end: 4 })).toBe(
      "docs/plan.md:2-4\n> two\n>\n> four",
    );
  });

  it("puts a backwards range in order and clamps it to the file", () => {
    expect(referenceHeader("a.ts", { start: 9, end: 3 })).toBe("a.ts:3-9");
    expect(formatReference("a.ts", lines, { start: 4, end: 99 })).toBe("a.ts:4-5\n> four\n> five");
  });

  it("keeps an absolute path absolute", () => {
    expect(formatReference("/Users/me/notes/x.md", lines, { start: 1, end: 1 })).toBe(
      "/Users/me/notes/x.md:1\n> one",
    );
  });

  it("splits non-adjacent ranges into blocks in file order", () => {
    expect(mergeRanges([{ start: 5, end: 5 }, { start: 1, end: 1 }, { start: 2, end: 2 }])).toEqual([
      { start: 1, end: 2 },
      { start: 5, end: 5 },
    ]);
    expect(
      formatReferences("t.csv", lines, [
        { start: 5, end: 5 },
        { start: 1, end: 1 },
      ]),
    ).toBe("t.csv:1\n> one\n\nt.csv:5\n> five");
  });

  it("counts CRLF lines the same as LF", () => {
    expect(splitLines("a\r\nb")).toEqual(["a", "b"]);
  });
});

describe("text kinds", () => {
  it("picks a renderer by name", () => {
    expect(textKind("docs/plan.md")).toBe("markdown");
    expect(textKind("data/x.CSV")).toBe("csv");
    expect(textKind("x.tsv")).toBe("tsv");
    expect(textKind("src/main.rs")).toBe("code");
    expect(textKind("Dockerfile")).toBe("code");
    expect(textKind("lib/a.rb")).toBe("code");
    expect(textKind("server.log")).toBe("plain");
    expect(textKind(".env")).toBe("plain");
  });

  it("agrees with the shared text-kind fixture", () => {
    // `text_kinds.json` is what `veld_core::files`'s lists are tested against too
    // (`text_kind_lists_match_the_shared_fixture`), so a kind added on one side
    // only fails one of the two tests. A drift here is a disabled row for a file
    // the pane would show, or an enabled one that opens into a refusal.
    const renders: Record<string, TextKind> = {
      markdown: "markdown",
      csv: "csv",
      tsv: "tsv",
      plain: "plain",
      code: "code",
      sourceOnly: "code",
    };
    for (const [key, kind] of Object.entries(renders)) {
      for (const ext of fixture[key as keyof typeof fixture]) {
        expect(viewableAsText(`dir/a.${ext.toUpperCase()}`), ext).toBe(true);
        expect(textKind(`dir/a.${ext}`), ext).toBe(kind);
      }
    }
    for (const name of fixture.codeNames) {
      expect(viewableAsText(`app/${name}`), name).toBe(true);
      expect(textKind(name), name).toBe("code");
    }
    // The other direction: nothing here that the daemon would refuse by name.
    const everyExt = new Set(Object.entries(fixture).flatMap(([k, v]) => (k === "codeNames" ? [] : v)));
    const ours = [...Object.keys(BY_EXTENSION), ...Object.keys(LANGUAGES), ...CODE_EXTENSIONS];
    for (const ext of ours) expect(everyExt.has(ext), `${ext} is not in text_kinds.json`).toBe(true);
    for (const name of CODE_NAMES) expect(fixture.codeNames, name).toContain(name);
    expect(Object.keys(fixture).sort(), "a new fixture key needs a renderer above").toEqual(
      [...Object.keys(renders), "codeNames"].sort(),
    );
    for (const no of [
      "shot.png", "logo.svg", "report.pdf", "Cargo.lock", "LICENSE", ".gitignore", "db.sqlite",
      "dockerfile",
    ]) {
      expect(viewableAsText(no), no).toBe(false);
    }
  });

  it("maps code to a curated grammar or to text", () => {
    expect(languageFor("a.ts")).toBe("tsx");
    expect(languageFor("a.jsx")).toBe("tsx");
    expect(languageFor("Makefile")).toBe("make");
    expect(languageFor("a.rb")).toBe("text");
  });

  it("names exactly the grammars the curated Shiki entry bundles", () => {
    // The two lists are the two halves of one decision; a grammar named here and
    // not bundled would fail to resolve in the code view.
    expect(Object.keys(bundledLanguages).sort()).toEqual(curatedLanguages());
  });
});

describe("CSV parsing", () => {
  it("keeps each row's source lines, across quoted newlines and blank lines", () => {
    const text = 'name,note\nada,"two\nlines"\n\nbob,plain\n';
    const t = parseTable(text, false);
    expect(t.header).toEqual(["name", "note"]);
    expect(t.rows.map((r) => [r.cells[0], r.startLine, r.endLine])).toEqual([
      ["ada", 2, 3],
      ["bob", 5, 5],
    ]);
  });

  it("parses TSV and counts ragged columns", () => {
    const t = parseTable("a\tb\n1\t2\t3\n", true);
    expect(t.columns).toBe(3);
    expect(t.rows[0].cells).toEqual(["1", "2", "3"]);
  });

  it("sorts numerically and stably without moving a row's lines", () => {
    const t = parseTable("n\n10\n2\n2\n", false);
    const order = sortedOrder(t.rows, { column: 0, dir: "asc" });
    expect(order.map((i) => t.rows[i].startLine)).toEqual([3, 4, 2]);
    const desc = sortedOrder(t.rows, { column: 0, dir: "desc" });
    expect(desc.map((i) => t.rows[i].cells[0])).toEqual(["10", "2", "2"]);
    expect(sortedOrder(t.rows, null)).toEqual([0, 1, 2]);
  });
});

describe("gutter markers", () => {
  it("is empty with nothing changed", () => {
    expect(changeMarkersCss(null, 10)).toBe("");
    expect(changeMarkersCss({ added: [], modified: [], deleted: [] }, 10)).toBe("");
  });

  it("marks added and modified lines and a deletion notch", () => {
    const css = changeMarkersCss({ added: [[2, 3]], modified: [[5, 5]], deleted: [7, 10] }, 10);
    expect(css).toContain('[data-column-number="2"],[data-gutter] [data-column-number="3"]{box-shadow');
    expect(css).toContain('[data-column-number="5"]{box-shadow');
    // git's `+7,0`: the lines went after line 7, so the notch is on its bottom
    // edge — as it is after the last line.
    expect(css).toContain(
      '[data-gutter] [data-column-number="7"],[data-gutter] [data-column-number="10"]{background-image',
    );
    expect(css).toContain("background-position:bottom");
    expect(css).not.toContain("background-position:top");
  });

  it("draws a deletion above the first line on that line's top edge", () => {
    const css = changeMarkersCss({ added: [], modified: [], deleted: [0] }, 10);
    expect(css).toBe(
      '[data-gutter] [data-column-number="1"]{background-image:linear-gradient(var(--veld-gutter-deleted,#e05a50),var(--veld-gutter-deleted,#e05a50));background-size:100% 2px;background-repeat:no-repeat;background-position:top;}',
    );
  });

  it("collapses a whole-file range to one selector per rule", () => {
    expect(changeMarkersCss({ added: [[1, 500]], modified: [], deleted: [] }, 500)).toBe(
      "[data-gutter] [data-column-number]{box-shadow:inset 3px 0 0 var(--veld-gutter-added,#3fbf7f);}\n" +
        "[data-line]{background-image:linear-gradient(var(--veld-line-added,rgba(63,191,127,0.13)),var(--veld-line-added,rgba(63,191,127,0.13)));}",
    );
  });

  it("tints the changed code rows, not only the gutter", () => {
    const css = changeMarkersCss({ added: [[2, 2]], modified: [[5, 5]], deleted: [] }, 10);
    expect(css).toContain('[data-line="2"]{background-image');
    expect(css).toContain("rgba(63,191,127");
    expect(css).toContain('[data-line="5"]{background-image');
    expect(css).toContain("rgba(90,162,224");
  });
});

describe("open requests", () => {
  it("accepts either wire spelling and fills in what the transport knows", () => {
    const got: unknown[] = [];
    const stop = onOpenFileRequest((r) => got.push(r));
    expect(dispatchOpenFile({ worktree_id: 3, path: "plan.md", line: 4, notify: true })).toBe(true);
    expect(dispatchOpenFile({ path: "a.md" }, 7, "sess")).toBe(true);
    expect(dispatchOpenFile({ worktreeId: 1, path: "" })).toBe(false);
    expect(dispatchOpenFile({ path: "x.md" })).toBe(false);
    stop();
    expect(got).toEqual([
      { worktreeId: 3, path: "plan.md", line: 4, notify: true },
      { worktreeId: 7, sessionId: "sess", path: "a.md", notify: false },
    ]);
  });

  it("drops a nonsense line rather than passing it on", () => {
    const got: { line?: number }[] = [];
    const stop = onOpenFileRequest((r) => got.push(r));
    dispatchOpenFile({ worktreeId: 1, path: "a.md", line: -2 });
    dispatchOpenFile({ worktreeId: 1, path: "a.md", line: 1.5 });
    stop();
    expect(got.map((r) => r.line)).toEqual([undefined, undefined]);
  });

  it("carries an open_url with notify", () => {
    const got: unknown[] = [];
    const stop = onOpenUrlRequest((r) => got.push(r));
    dispatchOpenUrl({ worktree_id: 2, session_id: "s1", url: "https://x.test", notify: true });
    dispatchOpenUrl({ worktree_id: 2, url: "" });
    stop();
    expect(got).toEqual([{ worktreeId: 2, sessionId: "s1", url: "https://x.test", notify: true }]);
  });
});

describe("change stops", () => {
  it("are every hunk's first line, deletions included, in file order and once each", () => {
    expect(
      changeStops({ added: [[8, 9]], modified: [[3, 4]], deleted: [3, 12, 40] }, 20),
    ).toEqual([3, 8, 12, 20]);
    expect(changeStops(null, 20)).toEqual([]);
    expect(changeStops({ added: [], modified: [], deleted: [] }, 20)).toEqual([]);
  });
});

describe("show deletions", () => {
  // Old: a b c d e      New: a B c e f
  // so b→B is a replacement, d is deleted, f is added.
  const diff = parseDiffFromFile(
    { name: "f.txt", contents: "a\nb\nc\nd\ne\n" },
    { name: "f.txt", contents: "a\nB\nc\ne\nf\n" },
  );
  const blocks = changeBlocks(diff);

  it("finds each block on both sides", () => {
    expect(blocks).toEqual([
      { newStart: 2, oldStart: 2, deletions: 1, additions: 1 },
      { newStart: 4, oldStart: 4, deletions: 1, additions: 0 },
      { newStart: 5, oldStart: 6, deletions: 0, additions: 1 },
    ]);
    expect(diffStops(blocks, 5)).toEqual([2, 4, 5]);
  });

  it("counts deleted rows above a line, landing on a block's deleted rows", () => {
    // Rows: a, -b, +B, c, -d, e, +f
    expect(rowOfLine(blocks, 1)).toBe(0);
    expect(rowOfLine(blocks, 2)).toBe(1);
    expect(rowOfLine(blocks, 3)).toBe(3);
    expect(rowOfLine(blocks, 4)).toBe(4);
    expect(rowOfLine(blocks, 5)).toBe(6);
  });

  it("jumps to a block's deleted rows when it removed something, else to the line", () => {
    expect(diffRowOf(blocks, 2, 5)).toEqual({ line: 2, side: "deletions" });
    expect(diffRowOf(blocks, 4, 5)).toEqual({ line: 4, side: "deletions" });
    expect(diffRowOf(blocks, 5, 5)).toEqual({ line: 5, side: "additions" });
    expect(diffRowOf(blocks, 3, 5)).toEqual({ line: 3, side: "additions" });
    // A deletion at the end stops on the last line, but lands on what went.
    const tail = changeBlocks(
      parseDiffFromFile({ name: "t.txt", contents: "a\nb\n" }, { name: "t.txt", contents: "a\n" }),
    );
    expect(diffStops(tail, 1)).toEqual([1]);
    expect(diffRowOf(tail, 1, 1)).toEqual({ line: 2, side: "deletions" });
  });

  it("trims a selection to the current lines it spans", () => {
    expect(newLineRange(blocks, { start: 1, end: 3, side: "additions" })).toEqual({ start: 1, end: 3 });
    // From deleted b down to c: starts at B.
    expect(
      newLineRange(blocks, { start: 2, side: "deletions", end: 3, endSide: "additions" }),
    ).toEqual({ start: 2, end: 3 });
    // From c down to deleted d: ends at c.
    expect(
      newLineRange(blocks, { start: 3, side: "additions", end: 4, endSide: "deletions" }),
    ).toEqual({ start: 3, end: 3 });
    // Dragged upwards, the same.
    expect(
      newLineRange(blocks, { start: 4, side: "deletions", end: 3, endSide: "additions" }),
    ).toEqual({ start: 3, end: 3 });
  });

  it("side by side, lands on the current row unless a block added nothing", () => {
    // b→B shares one row with what replaced it; the deleted d has no right-hand row.
    expect(diffRowOf(blocks, 2, 5, true)).toEqual({ line: 2, side: "additions" });
    expect(diffRowOf(blocks, 4, 5, true)).toEqual({ line: 4, side: "deletions" });
    expect(diffRowOf(blocks, 5, 5, true)).toEqual({ line: 5, side: "additions" });
  });

  it("reads an unchanged left-hand row by the current line it is", () => {
    // Side by side the left column reports a by old 1 and e by old 5 — now line 4.
    expect(newLineRange(blocks, { start: 1, end: 5, side: "deletions" })).toEqual({
      start: 1,
      end: 4,
    });
    // c (old 3) down to deleted d: ends at c.
    expect(newLineRange(blocks, { start: 3, end: 4, side: "deletions" })).toEqual({
      start: 3,
      end: 3,
    });
    // Left-hand a across to right-hand f.
    expect(
      newLineRange(blocks, { start: 1, side: "deletions", end: 5, endSide: "additions" }),
    ).toEqual({ start: 1, end: 5 });
  });

  it("has nothing to cite in deleted rows alone", () => {
    expect(newLineRange(blocks, { start: 4, end: 4, side: "deletions" })).toBeNull();
    expect(newLineRange(blocks, { start: 2, end: 2, side: "deletions" })).toBeNull();
  });
});
