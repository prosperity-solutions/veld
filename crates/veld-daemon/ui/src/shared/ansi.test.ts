import { describe, expect, it } from "vitest";
import {
  ansiCss,
  ansiIndexColor,
  ansiPalette,
  markAnsiSpans,
  parseAnsi,
  stripAnsi,
  type AnsiSpan,
} from "./ansi";

/** Spans as `[text, fg-index-or-null]`, which is what most cases care about. */
function shape(spans: AnsiSpan[]): Array<[string, number | string | null]> {
  return spans.map((s) => [
    s.text,
    s.style.fg
      ? s.style.fg.kind === "index"
        ? s.style.fg.index
        : `rgb(${s.style.fg.r},${s.style.fg.g},${s.style.fg.b})`
      : null,
  ]);
}

describe("parseAnsi", () => {
  it("leaves plain text as one unstyled span", () => {
    expect(parseAnsi("just a line")).toEqual([{ text: "just a line", style: {} }]);
  });

  it("returns nothing for empty input", () => {
    expect(parseAnsi("")).toEqual([]);
  });

  it("splits on colour and closes on reset", () => {
    expect(shape(parseAnsi("ok \x1b[31mbad\x1b[0m done"))).toEqual([
      ["ok ", null],
      ["bad", 1],
      [" done", null],
    ]);
  });

  it("maps the bright range to slots 8-15", () => {
    expect(shape(parseAnsi("\x1b[91mhot"))).toEqual([["hot", 9]]);
  });

  it("keeps attributes across an unrelated colour change", () => {
    const spans = parseAnsi("\x1b[1m\x1b[32mgreen bold");
    expect(spans).toHaveLength(1);
    expect(spans[0].style.bold).toBe(true);
    expect(spans[0].style.fg).toEqual({ kind: "index", index: 2 });
  });

  it("reads 256-colour and truecolor forms", () => {
    expect(shape(parseAnsi("\x1b[38;5;208morange"))).toEqual([["orange", 208]]);
    expect(shape(parseAnsi("\x1b[38;2;10;20;30mrgb"))).toEqual([["rgb", "rgb(10,20,30)"]]);
  });

  it("consumes an extended colour's sub-parameters instead of restyling on them", () => {
    // The trap: `38;2;1;2;3` read left to right sets fg, then *bold* (1), then
    // *dim* (2), then *italic* (3). Every one of those digits is part of the
    // colour.
    const spans = parseAnsi("\x1b[38;2;1;2;3mx");
    expect(spans[0].style).toEqual({ fg: { kind: "rgb", r: 1, g: 2, b: 3 } });
  });

  it("drops a malformed extended colour without styling the rest of the list", () => {
    // `38;6` is not a form anyone implements; the parameters after it belong to
    // it, so guessing at its length is worse than stopping.
    const spans = parseAnsi("\x1b[38;6;1mx");
    expect(spans[0].style).toEqual({});
  });

  it("treats an empty parameter as a reset, the way terminals do", () => {
    // `ESC [ m` and `ESC [ 0 m` are the same sequence.
    expect(shape(parseAnsi("\x1b[31mred\x1b[mplain"))).toEqual([
      ["red", 1],
      ["plain", null],
    ]);
  });

  it("undoes single attributes without clearing the rest", () => {
    const spans = parseAnsi("\x1b[1;4;31mboth\x1b[24mno underline");
    expect(spans[0].style.underline).toBe(true);
    expect(spans[1].style.underline).toBeUndefined();
    expect(spans[1].style.bold).toBe(true);
    expect(spans[1].style.fg).toEqual({ kind: "index", index: 1 });
  });

  it("merges runs that would render identically", () => {
    // A CLI that resets and re-sets the same colour per word is common; one span
    // per word would multiply the DOM for no visual difference.
    const spans = parseAnsi("\x1b[31ma\x1b[31mb\x1b[31mc");
    expect(spans).toHaveLength(1);
    expect(spans[0].text).toBe("abc");
  });

  it("drops non-SGR CSI sequences and keeps their text", () => {
    // Cursor moves, erases, scroll regions: a log line has no cursor.
    expect(stripAnsi("\x1b[2K\x1b[1;1Hcleared\x1b[?25l")).toBe("cleared");
  });

  it("drops OSC sequences terminated by BEL or by ST", () => {
    expect(stripAnsi("\x1b]0;a title\x07after")).toBe("after");
    expect(stripAnsi("\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\")).toBe("link");
  });

  it("survives an unterminated string escape without eating the line", () => {
    // A clipped line ends mid-OSC. The opener goes; nothing else should.
    const line = "\x1b]0;" + "x".repeat(5000) + "|tail";
    expect(stripAnsi(line).endsWith("|tail")).toBe(true);
  });

  it("drops a truncated CSI at the end of a line", () => {
    expect(stripAnsi("text\x1b[")).toBe("text");
    expect(stripAnsi("text\x1b")).toBe("text");
    expect(stripAnsi("text\x1b[3")).toBe("text");
  });

  it("ignores a private-parameter CSI ending in m", () => {
    // `ESC [ ? 1 m` is a private mode, not SGR — parsing its digits as
    // attributes would make it set bold.
    const spans = parseAnsi("\x1b[?1mplain");
    expect(spans[0].style).toEqual({});
    expect(spans[0].text).toBe("plain");
  });

  it("restarts the line at a carriage return", () => {
    // Progress output: every frame is in the line, and only the last one is what
    // the terminal showed.
    expect(stripAnsi("12%\r45%\r100%")).toBe("100%");
  });

  it("keeps the line when the carriage return is the last character", () => {
    // A trailing CR is a line ending, not an overwrite. Clearing on it would
    // blank every line of CRLF-terminated output.
    expect(stripAnsi("done\r")).toBe("done");
  });

  it("clears earlier spans on overwrite, not just the current one", () => {
    expect(shape(parseAnsi("\x1b[31mfirst\x1b[0m plain\rsecond"))).toEqual([["second", null]]);
  });

  it("drops other control characters and keeps tabs", () => {
    expect(stripAnsi("bell\x07 back\x08 del\x7f end")).toBe("bell back del end");
    expect(stripAnsi("a\tb")).toBe("a\tb");
  });

  it("agrees with itself: stripAnsi is the spans joined", () => {
    const line = "\x1b[1;31mERROR\x1b[0m \x1b[2Kfailed to \x1b[38;5;33mconnect\x1b[0m";
    expect(
      parseAnsi(line)
        .map((s) => s.text)
        .join(""),
    ).toBe(stripAnsi(line));
  });
});

describe("palette", () => {
  it("orders the 16 base colours the way SGR indexes them", () => {
    const dark = ansiPalette("dark");
    expect(dark).toHaveLength(16);
    expect(dark[1]).toBe("#e05a50"); // 31 = red
    expect(dark[9]).toBe("#ff7a70"); // 91 = bright red
    expect(ansiPalette("light")[1]).toBe("#c33c32");
  });

  it("computes the 6x6x6 cube with xterm's uneven levels", () => {
    expect(ansiIndexColor(16, "dark")).toBe("rgb(0, 0, 0)");
    expect(ansiIndexColor(21, "dark")).toBe("rgb(0, 0, 255)");
    expect(ansiIndexColor(196, "dark")).toBe("rgb(255, 0, 0)");
    // The levels are not linear: slot 1 is 95, not 51.
    expect(ansiIndexColor(17, "dark")).toBe("rgb(0, 0, 95)");
  });

  it("computes the grey ramp and refuses what is out of range", () => {
    expect(ansiIndexColor(232, "dark")).toBe("rgb(8, 8, 8)");
    expect(ansiIndexColor(255, "dark")).toBe("rgb(238, 238, 238)");
    expect(ansiIndexColor(256, "dark")).toBeUndefined();
    expect(ansiIndexColor(-1, "dark")).toBeUndefined();
  });
});

describe("ansiCss", () => {
  it("is empty for an unstyled run", () => {
    expect(ansiCss({}, "dark")).toEqual({});
  });

  it("resolves colours by theme", () => {
    expect(ansiCss({ fg: { kind: "index", index: 1 } }, "dark").color).toBe("#e05a50");
    expect(ansiCss({ fg: { kind: "index", index: 1 } }, "light").color).toBe("#c33c32");
  });

  it("swaps the pair for inverse, standing in the panel's colours", () => {
    const css = ansiCss({ inverse: true, fg: { kind: "index", index: 2 } }, "dark");
    expect(css.background).toBe("#3fbf7f");
    expect(css.color).toBe("var(--panel)");
  });

  it("combines both text decorations", () => {
    expect(ansiCss({ underline: true, strike: true }, "dark").textDecoration).toBe(
      "underline line-through",
    );
  });
});

describe("markAnsiSpans", () => {
  it("marks nothing without a term", () => {
    const spans = parseAnsi("\x1b[31mred\x1b[0m plain");
    expect(markAnsiSpans(spans, "").every((p) => !p.mark)).toBe(true);
  });

  it("splits a span around a match and keeps its style", () => {
    const pieces = markAnsiSpans(parseAnsi("\x1b[31mfailed to connect"), "to");
    expect(pieces.map((p) => [p.text, p.mark])).toEqual([
      ["failed ", false],
      ["to", true],
      [" connect", false],
    ]);
    expect(pieces.every((p) => p.style.fg?.kind === "index")).toBe(true);
  });

  it("matches a term that straddles a style change", () => {
    // The whole reason matching runs over the joined text: `ERROR` bold and the
    // rest plain is the commonest colouring there is, and a per-span search
    // cannot see a word that crosses the boundary.
    const pieces = markAnsiSpans(parseAnsi("\x1b[1mERR\x1b[0mOR here"), "error");
    const marked = pieces.filter((p) => p.mark);
    expect(marked.map((p) => p.text)).toEqual(["ERR", "OR"]);
    expect(marked[0].style.bold).toBe(true);
    expect(marked[1].style.bold).toBeUndefined();
  });

  it("is case-insensitive and finds every occurrence", () => {
    const pieces = markAnsiSpans(parseAnsi("Err err ERR"), "err");
    expect(pieces.filter((p) => p.mark)).toHaveLength(3);
  });

  it("does not produce overlapping pieces for a repeating term", () => {
    // "aa" in "aaaa" is two matches, not three: the ranges must be disjoint or
    // the splitter emits text twice.
    const pieces = markAnsiSpans(parseAnsi("aaaa"), "aa");
    expect(pieces.map((p) => p.text).join("")).toBe("aaaa");
    expect(pieces.filter((p) => p.mark)).toHaveLength(2);
  });

  it("never loses or duplicates text", () => {
    const line = "\x1b[1;31mERROR\x1b[0m \x1b[36mconnect\x1b[0m failed: connect";
    for (const term of ["", "e", "connect", "ERROR failed", "zzz"]) {
      const pieces = markAnsiSpans(parseAnsi(line), term);
      expect(pieces.map((p) => p.text).join("")).toBe(stripAnsi(line));
    }
  });
});
