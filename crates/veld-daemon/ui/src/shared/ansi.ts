// ANSI escape handling for text that was written for a terminal and is being
// rendered as HTML — today the logs panel, whose lines come from a CLI that
// colours its own output (`crates/veld/src/output.rs`) and from dev servers that
// colour far more.
//
// Two jobs, deliberately separate:
//
//   `parseAnsi`  — SGR (colour/bold/underline…) becomes structure the renderer
//                  can style, and every *other* escape sequence is dropped.
//   `stripAnsi`  — the same text with nothing but the characters, which is what
//                  searching and matching must run against. Searching the raw
//                  line silently fails whenever an escape sits inside the word
//                  being looked for, which is exactly what colour does to the
//                  word `error`.
//
// The parser is deliberately not a terminal: it has no cursor, no scroll region
// and no grid. A log line is a line. The one cursor movement it honours is
// carriage return, because progress output ("12%\r34%\r…") is otherwise rendered
// as every frame of the animation at once.

/** A colour as SGR expresses it: a palette slot, or a literal RGB triple. */
export type AnsiColor =
  | { kind: "index"; index: number }
  | { kind: "rgb"; r: number; g: number; b: number };

/** Everything SGR can say about a run of text. All fields optional so an
 *  unstyled run is `{}` and cheap to compare. */
export interface AnsiStyle {
  fg?: AnsiColor;
  bg?: AnsiColor;
  bold?: boolean;
  dim?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
  /** SGR 7. Resolved at render time, since it needs the surface colours. */
  inverse?: boolean;
}

/** A run of text with one style. */
export interface AnsiSpan {
  text: string;
  style: AnsiStyle;
}

/**
 * ANSI palettes.
 *
 * These are terminal colours, not design tokens — programs address them by
 * index, so they cannot come from the theme variables. They live here rather
 * than beside xterm because there are now two consumers (the terminal and the
 * logs panel) and a log line coloured differently from the same output in a
 * shell would read as a bug in one of them.
 *
 * Order matters: SGR 30–37 index this list, 90–97 index its bright half, and
 * `ansiPalette` flattens it in exactly that order.
 */
export const ANSI_DARK = {
  black: "#22262c",
  red: "#e05a50",
  green: "#3fbf7f",
  yellow: "#e6b43c",
  blue: "#5aa2e0",
  magenta: "#b98ce0",
  cyan: "#4fbfc0",
  white: "#c8ced5",
  brightBlack: "#666d76",
  brightRed: "#ff7a70",
  brightGreen: "#5fdf9f",
  brightYellow: "#ffd45c",
  brightBlue: "#7ac2ff",
  brightMagenta: "#d9acff",
  brightCyan: "#6fdfe0",
  brightWhite: "#f2f4f6",
};

export const ANSI_LIGHT = {
  black: "#2b3138",
  red: "#c33c32",
  green: "#28965f",
  yellow: "#a5761a",
  blue: "#2f6fb5",
  magenta: "#8a54b8",
  cyan: "#1f8a8c",
  white: "#5c636b",
  brightBlack: "#98a0a9",
  brightRed: "#e05a50",
  brightGreen: "#3fbf7f",
  brightYellow: "#c68f20",
  brightBlue: "#3f8ad0",
  brightMagenta: "#a06cd0",
  brightCyan: "#2aa5a7",
  brightWhite: "#171a1d",
};

/** The 16 base colours in SGR index order. */
export function ansiPalette(theme: "dark" | "light"): string[] {
  const p = theme === "dark" ? ANSI_DARK : ANSI_LIGHT;
  return [
    p.black,
    p.red,
    p.green,
    p.yellow,
    p.blue,
    p.magenta,
    p.cyan,
    p.white,
    p.brightBlack,
    p.brightRed,
    p.brightGreen,
    p.brightYellow,
    p.brightBlue,
    p.brightMagenta,
    p.brightCyan,
    p.brightWhite,
  ];
}

/**
 * A 256-colour index as CSS.
 *
 * 0–15 are the palette; 16–231 are a 6×6×6 RGB cube with the levels xterm uses
 * (0, 95, 135, 175, 215, 255 — deliberately not evenly spaced); 232–255 are a
 * 24-step grey ramp. Out of range falls back to inheriting, which is what an
 * unparsable colour should do.
 */
export function ansiIndexColor(index: number, theme: "dark" | "light"): string | undefined {
  if (index < 0 || index > 255) return undefined;
  if (index < 16) return ansiPalette(theme)[index];
  if (index < 232) {
    const n = index - 16;
    const levels = [0, 95, 135, 175, 215, 255];
    const r = levels[Math.floor(n / 36) % 6];
    const g = levels[Math.floor(n / 6) % 6];
    const b = levels[n % 6];
    return `rgb(${r}, ${g}, ${b})`;
  }
  const grey = 8 + (index - 232) * 10;
  return `rgb(${grey}, ${grey}, ${grey})`;
}

/** Resolve one colour to CSS. */
export function ansiColorCss(color: AnsiColor, theme: "dark" | "light"): string | undefined {
  return color.kind === "rgb"
    ? `rgb(${color.r}, ${color.g}, ${color.b})`
    : ansiIndexColor(color.index, theme);
}

/**
 * How far to scan for the terminator of a string-type escape (OSC, DCS, APC…).
 *
 * A truncated log line can contain an opener with no terminator — a dev server
 * killed mid-write, or a line the daemon clipped. Without a bound, one such line
 * swallows the rest of itself; with it, the opener is dropped and the text after
 * the bound survives. 4 KB is far past any real OSC (a hyperlink or a title) and
 * far below a line worth losing.
 */
const STRING_ESCAPE_LIMIT = 4096;

/** Where a string-type escape ends: after BEL, after ST (`ESC \`), or at the
 *  scan limit. Returns the index *after* the terminator. */
function endOfStringEscape(text: string, from: number): number {
  const limit = Math.min(text.length, from + STRING_ESCAPE_LIMIT);
  for (let i = from; i < limit; i++) {
    if (text[i] === "\x07") return i + 1;
    if (text[i] === "\x1b" && text[i + 1] === "\\") return i + 2;
  }
  return limit;
}

/** Any C0 control or DEL — including `\t` and `\n`, which the parser keeps but
 *  which must still take the slow path so the fast one can be a pure "no control
 *  characters at all" test rather than a second copy of the keep/drop rules.
 *  Not a global regex: `lastIndex` would carry between calls. Written with
 *  escapes, not the characters themselves: a literal control byte in a source
 *  file makes it a binary file to half the tools that read it. */
const CONTROL_CHARS = /[\x00-\x1f\x7f]/;

/** Apply one SGR parameter list to `style`, in place. */
function applySgr(style: AnsiStyle, params: number[]): void {
  for (let i = 0; i < params.length; i++) {
    const p = params[i];
    switch (true) {
      case p === 0:
        for (const key of Object.keys(style) as Array<keyof AnsiStyle>) delete style[key];
        break;
      case p === 1:
        style.bold = true;
        break;
      case p === 2:
        style.dim = true;
        break;
      case p === 3:
        style.italic = true;
        break;
      case p === 4:
        style.underline = true;
        break;
      case p === 7:
        style.inverse = true;
        break;
      case p === 9:
        style.strike = true;
        break;
      // 21 is "double underline" in ECMA-48 and "bold off" in most terminals;
      // either way it ends the bold run, which is all this renderer tracks.
      case p === 21 || p === 22:
        delete style.bold;
        delete style.dim;
        break;
      case p === 23:
        delete style.italic;
        break;
      case p === 24:
        delete style.underline;
        break;
      case p === 27:
        delete style.inverse;
        break;
      case p === 29:
        delete style.strike;
        break;
      case p >= 30 && p <= 37:
        style.fg = { kind: "index", index: p - 30 };
        break;
      case p >= 40 && p <= 47:
        style.bg = { kind: "index", index: p - 40 };
        break;
      case p >= 90 && p <= 97:
        style.fg = { kind: "index", index: p - 90 + 8 };
        break;
      case p >= 100 && p <= 107:
        style.bg = { kind: "index", index: p - 100 + 8 };
        break;
      case p === 39:
        delete style.fg;
        break;
      case p === 49:
        delete style.bg;
        break;
      case p === 38 || p === 48: {
        // Extended colour: `38;5;n` (256) or `38;2;r;g;b` (truecolor). The
        // sub-parameters are consumed here, so an unrecognised form must still
        // advance `i` past them — otherwise its digits get read as separate
        // attributes and a truecolor sequence sets bold and italic on the way
        // past.
        const target: keyof AnsiStyle = p === 38 ? "fg" : "bg";
        const mode = params[i + 1];
        if (mode === 5 && params.length > i + 2) {
          style[target] = { kind: "index", index: params[i + 2] };
          i += 2;
        } else if (mode === 2 && params.length > i + 4) {
          style[target] = {
            kind: "rgb",
            r: params[i + 2] & 0xff,
            g: params[i + 3] & 0xff,
            b: params[i + 4] & 0xff,
          };
          i += 4;
        } else {
          // Malformed or a form this does not implement (`38;6;…`): drop the
          // rest of the parameter list rather than guess at its length.
          return;
        }
        break;
      }
      default:
        // Everything else — 5 (blink), 53 (overline), 58 (underline colour) — is
        // parsed and ignored. Ignoring is the point: it must not reach the text.
        break;
    }
  }
}

/**
 * Split text into styled spans, dropping every escape sequence that is not SGR.
 *
 * Adjacent runs with the same style are merged, so a line that resets and
 * re-sets the same colour between every word does not become one span per word.
 */
export function parseAnsi(text: string): AnsiSpan[] {
  // Fast path for the overwhelmingly common line. Every branch below is entered
  // by a control character, so a line with none of them is one unstyled span —
  // and the panel parses every line of every node on every 2s poll.
  if (!CONTROL_CHARS.test(text)) return text ? [{ text, style: {} }] : [];

  const spans: AnsiSpan[] = [];
  let style: AnsiStyle = {};
  let buffer = "";

  const flush = () => {
    if (!buffer) return;
    const last = spans[spans.length - 1];
    if (last && sameStyle(last.style, style)) last.text += buffer;
    else spans.push({ text: buffer, style: { ...style } });
    buffer = "";
  };

  let i = 0;
  while (i < text.length) {
    const ch = text[i];

    if (ch === "\x1b") {
      const next = text[i + 1];
      if (next === "[") {
        // CSI: parameter bytes 0x30–0x3f, intermediates 0x20–0x2f, final 0x40–0x7e.
        let j = i + 2;
        while (j < text.length && text.charCodeAt(j) >= 0x30 && text.charCodeAt(j) <= 0x3f) j++;
        const paramEnd = j;
        while (j < text.length && text.charCodeAt(j) >= 0x20 && text.charCodeAt(j) <= 0x2f) j++;
        const final = text[j];
        if (final === undefined) {
          // Truncated at the end of the line: nothing to render, nothing to do.
          i = text.length;
          continue;
        }
        if (final === "m") {
          flush();
          const raw = text.slice(i + 2, paramEnd);
          // A private-parameter CSI (`ESC [ ? … m`) is not SGR; ignore it rather
          // than parsing its digits as attributes.
          if (!/^[<=>?]/.test(raw)) {
            const params = raw
              .split(";")
              .map((p) => (p === "" ? 0 : Number.parseInt(p, 10)))
              .map((n) => (Number.isNaN(n) ? 0 : n));
            style = { ...style };
            applySgr(style, params.length ? params : [0]);
          }
        }
        // Every other final byte — cursor moves, erases, scroll regions — is
        // dropped. A log line is a line; there is nowhere for a cursor to go.
        i = j + 1;
        continue;
      }
      if (next === "]" || next === "P" || next === "X" || next === "^" || next === "_") {
        i = endOfStringEscape(text, i + 2);
        continue;
      }
      if (next === undefined) {
        i = text.length;
        continue;
      }
      // Two-character escape: charset selection, RIS, keypad mode, NEL…
      i += 2;
      continue;
    }

    // Carriage return: the line starts over. Progress output relies on this, and
    // rendering every frame of it is how a 40-step progress bar becomes one
    // 800-character line. A *trailing* CR is a line ending, not an overwrite, so
    // it is dropped without discarding the line.
    if (ch === "\r") {
      if (i + 1 < text.length) {
        buffer = "";
        spans.length = 0;
      }
      i++;
      continue;
    }

    // Remaining C0 controls (BEL, backspace, form feed, the ESC-less C1 range)
    // have no rendering here. Tab and newline are kept: the log row preserves
    // whitespace, and both mean what they say.
    const code = text.charCodeAt(i);
    if ((code < 0x20 && ch !== "\t" && ch !== "\n") || code === 0x7f) {
      i++;
      continue;
    }

    buffer += ch;
    i++;
  }
  flush();
  return spans;
}

/** Whether two styles would render identically. */
function sameStyle(a: AnsiStyle, b: AnsiStyle): boolean {
  return (
    sameColor(a.fg, b.fg) &&
    sameColor(a.bg, b.bg) &&
    !a.bold === !b.bold &&
    !a.dim === !b.dim &&
    !a.italic === !b.italic &&
    !a.underline === !b.underline &&
    !a.strike === !b.strike &&
    !a.inverse === !b.inverse
  );
}

function sameColor(a: AnsiColor | undefined, b: AnsiColor | undefined): boolean {
  if (!a || !b) return !a && !b;
  if (a.kind !== b.kind) return false;
  return a.kind === "index" && b.kind === "index"
    ? a.index === b.index
    : a.kind === "rgb" && b.kind === "rgb" && a.r === b.r && a.g === b.g && a.b === b.b;
}

/**
 * The text with every escape sequence removed — what the user sees, and
 * therefore the only correct thing to search.
 *
 * Defined as the concatenation of `parseAnsi`'s spans on purpose: two
 * implementations of "what does this line say" drift, and a search that matches
 * text no span contains highlights nothing while claiming a hit.
 */
export function stripAnsi(text: string): string {
  return parseAnsi(text)
    .map((s) => s.text)
    .join("");
}

/** The subset of CSS an SGR run can produce. A plain object rather than React's
 *  `CSSProperties` so this module stays renderer-agnostic and testable in the
 *  `node` environment the suite runs in. */
export interface AnsiCss {
  color?: string;
  background?: string;
  fontWeight?: number;
  fontStyle?: string;
  textDecoration?: string;
  opacity?: number;
}

/**
 * One style as CSS.
 *
 * `inverse` is resolved here rather than at parse time because it needs the
 * surface colours, which are the panel's design tokens — the same reason the
 * palette is not baked into the parser.
 */
export function ansiCss(style: AnsiStyle, theme: "dark" | "light"): AnsiCss {
  const css: AnsiCss = {};
  const fg = style.fg ? ansiColorCss(style.fg, theme) : undefined;
  const bg = style.bg ? ansiColorCss(style.bg, theme) : undefined;
  if (style.inverse) {
    // Swapped, with the panel's own colours standing in for whichever side the
    // line never set — which is what a terminal does with its default pair.
    css.color = bg ?? "var(--panel)";
    css.background = fg ?? "var(--text)";
  } else {
    if (fg) css.color = fg;
    if (bg) css.background = bg;
  }
  if (style.bold) css.fontWeight = 600;
  if (style.italic) css.fontStyle = "italic";
  const decorations = [style.underline ? "underline" : "", style.strike ? "line-through" : ""]
    .filter(Boolean)
    .join(" ");
  if (decorations) css.textDecoration = decorations;
  // Dim is the one attribute with no faithful CSS equivalent: a terminal picks a
  // dimmer *colour*, which cannot be done to an arbitrary one without knowing the
  // background. Opacity over the panel is the closest thing that also works when
  // the colour came from a 24-bit sequence.
  if (style.dim) css.opacity = 0.7;
  return css;
}

/** A span split by a search term: `mark` pieces are the matches. */
export interface AnsiPiece extends AnsiSpan {
  mark: boolean;
}

/**
 * Mark every occurrence of `term` across `spans`, splitting spans where a match
 * starts or ends.
 *
 * Matching runs over the *joined* text, not span by span, because a colour
 * change inside a word is common (`ERROR` bold, the message plain) and a
 * per-span search would not find a term that straddles one.
 */
export function markAnsiSpans(spans: AnsiSpan[], term: string): AnsiPiece[] {
  if (!term) return spans.map((s) => ({ ...s, mark: false }));
  const text = spans.map((s) => s.text).join("");
  const ranges = matchRanges(text, term);
  if (ranges.length === 0) return spans.map((s) => ({ ...s, mark: false }));

  // Both lists are sorted and disjoint, so this is a merge walk: each span
  // advances a shared range cursor rather than rescanning the whole range list.
  // The obvious nested loop is quadratic, and it is not a theoretical concern —
  // one heavily-coloured megabyte line (142k spans, 71k matches) took **15.7
  // seconds** in it, which is a frozen tab on the first keystroke of a search.
  // The old `highlight` this replaced was a single `String.split`, so anything
  // superlinear here is a regression.
  const pieces: AnsiPiece[] = [];
  let at = 0;
  let first = 0;
  for (const span of spans) {
    const start = at;
    const end = at + span.text.length;
    at = end;
    // Ranges that end at or before this span's start can never be seen again.
    // Advanced on `first` — not on the inner cursor — because a range that
    // *straddles* the boundary must still be visible to the next span.
    while (first < ranges.length && ranges[first][1] <= start) first++;
    let cursor = start;
    for (let k = first; k < ranges.length && ranges[k][0] < end; k++) {
      const matchFrom = Math.max(ranges[k][0], start);
      const matchTo = Math.min(ranges[k][1], end);
      if (matchFrom > cursor) {
        pieces.push({ text: text.slice(cursor, matchFrom), style: span.style, mark: false });
      }
      pieces.push({ text: text.slice(matchFrom, matchTo), style: span.style, mark: true });
      cursor = matchTo;
    }
    if (cursor < end) {
      pieces.push({ text: text.slice(cursor, end), style: span.style, mark: false });
    }
  }
  return pieces.filter((p) => p.text !== "");
}

/** Case-insensitive literal occurrences of `term`, as [start, end) pairs. */
function matchRanges(text: string, term: string): Array<[number, number]> {
  const haystack = text.toLowerCase();
  const needle = term.toLowerCase();
  const out: Array<[number, number]> = [];
  let from = 0;
  for (;;) {
    const at = haystack.indexOf(needle, from);
    if (at === -1) return out;
    out.push([at, at + needle.length]);
    // `at + length`, never `at + 1`: overlapping matches would produce
    // overlapping ranges, and the splitter above assumes they are disjoint.
    from = at + needle.length;
  }
}
