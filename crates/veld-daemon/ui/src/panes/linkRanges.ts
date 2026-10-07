/**
 * Turning a terminal's wrapped rows into one logical line, and an offset in that
 * line back into a cell.
 *
 * Split out of `terminalHost.ts` and kept DOM-free — the same reason `dropModel.ts`
 * and `terminalKeys.ts` are — because this is arithmetic with an off-by-one at every
 * boundary (1-based vs 0-based, inclusive vs exclusive) and none of it is testable
 * through a rendered xterm.
 */

/**
 * One terminal cell, as xterm's public `IBufferCell` reports it: what it holds
 * (`getChars()`, `""` for an empty cell) and how many columns it takes
 * (`getWidth()`: 1, 2 for a wide character, 0 for the second half of one).
 */
export interface Cell {
  chars: string;
  width: number;
}

/** One visual row, as much of it as this module needs. */
export interface CellRow {
  cells: Cell[];
  /** True when this row is the continuation of the one above it. */
  isWrapped: boolean;
}

/** A cell in xterm's coordinates: **1-based** in both axes. */
export interface LinkCell {
  x: number;
  y: number;
}

/** Where one UTF-16 unit of a block's text sits: its cell, and that cell's width. */
interface Placed extends LinkCell {
  width: number;
}

/**
 * How many rows of one wrapped block this will assemble — **the count, not an
 * index**: a block of exactly this many rows is assembled, one more is declined.
 *
 * A logical line is normally a handful of rows, and then somebody prints a minified
 * bundle, a base64 blob or a single-line JSON log. With `terminal.scrollback` at its
 * default of 10000, that is one block of ten thousand rows — and xterm re-asks the
 * provider on **every row the pointer crosses** (it caches only same-row moves), so
 * dragging down a viewport would rebuild an ~800 KB string forty times on the main
 * thread. No path is wrapped across more rows than this.
 */
const MAX_BLOCK_ROWS = 256;

/**
 * A logical line: its text, and for every UTF-16 unit of it the cell it came from.
 *
 * The map is what lets a match found in the joined text be drawn on the right
 * cells. It used to be arithmetic (`offset % cols`), which is only true while every
 * string character is exactly one column — and that refused the whole block the
 * moment it held one full-width character, so a path on any line with a ✅, a CJK
 * name or a wide glyph in it never linked, wrapped or not.
 */
export interface LogicalBlock {
  text: string;
  /** 0-based row the block starts on. */
  startY: number;
  /** 0-based row it ends on. */
  endY: number;
  at: Placed[];
  /**
   * Offsets in `text` where a hard line break was removed ({@link hardWrappedBlockAt}).
   * A link is split at these, so each printed row's piece underlines on its own
   * and the indentation between them does not.
   */
  breaks: number[];
}

/**
 * The whole logical line that row `asked` belongs to, or `null` past the buffer or
 * past {@link MAX_BLOCK_ROWS}.
 *
 * # Why assemble at all
 *
 * A terminal wraps, and `crates/veld-daemon/src/extensions.rs:445` is 44 characters:
 * a narrow pane breaks it across two rows, and a matcher reading one row sees two
 * fragments and links neither. So rows are joined while the next one `isWrapped`,
 * after walking back to the row that began the line.
 *
 * # Reading cells rather than `translateToString`
 *
 * Each cell is placed by its own column, so wide characters (`width` 2, then a
 * `width` 0 placeholder that is skipped) and combining sequences (several units in
 * one cell) map exactly. One more case only cells can see: a wide character that
 * does not fit in a row's last column is moved to the next row and leaves that
 * column **empty** — `translateToString` turned it into a space in the middle of the
 * logical line, splitting a wrapped path in two. Empty cells at the end of a row
 * that wraps are therefore dropped; elsewhere an empty cell is a space, as on screen.
 */
export function logicalBlockAt(
  getRow: (y: number) => CellRow | undefined,
  asked: number,
): LogicalBlock | null {
  if (asked < 0 || !getRow(asked)) {
    return null;
  }
  let startY = asked;
  // A wrapped row continues the one above, so walk back to the row that began it —
  // but only so far. Past the cap the answer is "no links here".
  let back = 0;
  while (startY > 0 && getRow(startY)?.isWrapped) {
    if (++back >= MAX_BLOCK_ROWS) {
      return null;
    }
    startY -= 1;
  }
  let text = "";
  const at: Placed[] = [];
  for (let y = startY; ; y += 1) {
    const row = getRow(y);
    if (!row || (y > startY && !row.isWrapped)) {
      // The block ended on its own, within the cap. This is the only success exit.
      return { text, startY, endY: y - 1, at, breaks: [] };
    }
    // Checked *before* appending and only once a row is known to belong to the
    // block, so a block of exactly `MAX_BLOCK_ROWS` assembles and one of
    // `MAX_BLOCK_ROWS + 1` declines.
    if (y - startY >= MAX_BLOCK_ROWS) {
      return null;
    }
    const wrapsOn = getRow(y + 1)?.isWrapped === true;
    let last = row.cells.length;
    if (wrapsOn) {
      while (last > 0 && row.cells[last - 1].chars === "" && row.cells[last - 1].width === 1) {
        last -= 1;
      }
    }
    for (let x = 0; x < last; x += 1) {
      const cell = row.cells[x];
      if (cell.width === 0) continue; // The second half of a wide character.
      const chars = cell.chars === "" ? " " : cell.chars;
      text += chars;
      for (let i = 0; i < chars.length; i += 1) {
        at.push({ x: x + 1, y: y + 1, width: cell.width });
      }
    }
  }
}

/**
 * The xterm range for `text.slice(start, end)` of a block: 1-based, `end` inclusive
 * and on the *last column* the final character covers — a wide character's second
 * cell, so its underline is not cut in half.
 */
export function rangeOf(
  block: LogicalBlock,
  start: number,
  end: number,
): { start: LinkCell; end: LinkCell } | null {
  const first = block.at[start];
  const last = block.at[end - 1];
  if (!first || !last || end <= start) return null;
  return {
    start: { x: first.x, y: first.y },
    end: { x: last.x + Math.max(1, last.width) - 1, y: last.y },
  };
}

/**
 * How many hard line breaks one path may be joined across, each way. A real path
 * wraps a few times at most; this bounds the work per hovered row.
 */
const MAX_HARD_JOINS = 8;

/**
 * Whether two adjacent printed lines meet in the middle of one path — the decision
 * is `continuesPath`'s (in `filePaths.ts`, beside the rest of the matching rules);
 * this only finds the two tokens.
 */
export type JoinTest = (tail: string, head: string) => boolean;

function lastToken(text: string): string | null {
  return /(\S+)\s*$/u.exec(text)?.[1] ?? null;
}

function firstToken(text: string): string | null {
  return /^\s*(\S+)/u.exec(text)?.[1] ?? null;
}

/**
 * Join `next` onto `prev` across a hard break: `prev` with its trailing whitespace
 * dropped, `next` with its indentation dropped, and the seam recorded.
 */
function joinBlocks(prev: LogicalBlock, next: LogicalBlock): LogicalBlock {
  const keep = prev.text.trimEnd().length;
  const skip = next.text.length - next.text.trimStart().length;
  return {
    text: prev.text.slice(0, keep) + next.text.slice(skip),
    startY: prev.startY,
    endY: next.endY,
    at: [...prev.at.slice(0, keep), ...next.at.slice(skip)],
    breaks: [...prev.breaks, keep, ...next.breaks.map((b) => b - skip + keep)],
  };
}

/**
 * The logical line row `asked` belongs to, extended across **hard** line breaks
 * that split a path — the printer's own newline, not the terminal's wrap.
 *
 * An agent that wraps its output itself (Claude Code does, indenting each
 * continuation) breaks a long path with a real newline, so no row is `isWrapped`
 * and {@link logicalBlockAt} sees three unrelated lines. This walks to neighbouring
 * logical lines for as long as `joins` says the seam is inside one path, in both
 * directions, and joins them; every other token on those lines is unaffected, and
 * a seam `joins` refuses leaves the plain logical line.
 */
export function hardWrappedBlockAt(
  getRow: (y: number) => CellRow | undefined,
  asked: number,
  joins: JoinTest,
): LogicalBlock | null {
  const own = logicalBlockAt(getRow, asked);
  if (!own) return null;
  const seam = (prev: LogicalBlock, next: LogicalBlock) => {
    const tail = lastToken(prev.text);
    const head = firstToken(next.text);
    return tail !== null && head !== null && joins(tail, head);
  };
  let block = own;
  for (let i = 0; i < MAX_HARD_JOINS && block.startY > 0; i += 1) {
    const prev = logicalBlockAt(getRow, block.startY - 1);
    if (!prev || !seam(prev, block)) break;
    block = joinBlocks(prev, block);
  }
  for (let i = 0; i < MAX_HARD_JOINS; i += 1) {
    const next = logicalBlockAt(getRow, block.endY + 1);
    if (!next || !seam(block, next)) break;
    block = joinBlocks(block, next);
  }
  return block;
}

/**
 * The xterm ranges a match covers: one per piece between hard breaks, each from
 * {@link rangeOf}. A match with no break inside it is one range, as before —
 * soft-wrapped rows stay one link that underlines across them.
 */
export function rangesOf(
  block: LogicalBlock,
  start: number,
  end: number,
): { start: LinkCell; end: LinkCell }[] {
  const cuts = block.breaks.filter((b) => b > start && b < end);
  const out: { start: LinkCell; end: LinkCell }[] = [];
  let from = start;
  for (const cut of [...cuts, end]) {
    const r = rangeOf(block, from, cut);
    if (r) out.push(r);
    from = cut;
  }
  return out;
}
