/**
 * Turning a terminal's wrapped rows into one logical line, and an offset in that
 * line back into a cell.
 *
 * Split out of `terminalHost.ts` and kept DOM-free — the same reason `dropModel.ts`
 * and `terminalKeys.ts` are — because this is arithmetic with an off-by-one at every
 * boundary (1-based vs 0-based, inclusive vs exclusive) and none of it is testable
 * through a rendered xterm.
 */

/** One visual row, as much of it as this module needs. */
export interface WrappedRow {
  /** The row's full width, untrimmed — `translateToString(false)`. */
  text: string;
  /** True when this row is the continuation of the one above it. */
  isWrapped: boolean;
}

/** A cell in xterm's coordinates: **1-based** in both axes. */
export interface LinkCell {
  x: number;
  y: number;
}

/**
 * How many rows of one wrapped block this will assemble.
 *
 * A logical line is normally a handful of rows, and then somebody prints a minified
 * bundle, a base64 blob or a single-line JSON log. With `terminal.scrollback` at its
 * default of 10000, that is one block of ten thousand rows — and xterm re-asks the
 * provider on **every row the pointer crosses** (it caches only same-row moves), so
 * dragging down a viewport would rebuild an ~800 KB string forty times on the main
 * thread. No path is wrapped across more rows than this.
 */
const MAX_BLOCK_ROWS = 256;

/** A logical line, and the 0-based row it starts on. */
export interface LogicalBlock {
  text: string;
  startY: number;
}

/**
 * The whole logical line that row `asked` belongs to, or `null` when its geometry
 * cannot be trusted.
 *
 * # Why assemble at all
 *
 * A terminal wraps, and `crates/veld-daemon/src/extensions.rs:445` is 44 characters:
 * a narrow pane breaks it across two rows, and a matcher reading one row sees two
 * fragments and links neither.
 *
 * # Why it can return `null`
 *
 * Every offset→cell conversion here assumes **one string character per column**.
 * That holds for ASCII and for emoji (a non-BMP emoji is two UTF-16 units and two
 * cells, so it cancels out), and it fails for a full-width BMP character: CJK gives
 * one character for two cells, and a combining sequence several characters for one.
 * `translateToString` appends one cell's content per iteration while advancing by
 * that cell's *width*, so a row holding such a character comes back shorter than
 * `cols` — which is exactly the signal, and needs no access to xterm's private
 * `outColumns` parameter (it is in the implementation but not the public typings,
 * so reading it would couple this to a signature that can change unannounced).
 *
 * When that happens the honest answer is **no links on this block** rather than
 * links drawn in the wrong place: a path that does not underline costs a
 * copy-paste, an underline over the wrong characters costs trust in every other
 * one. That is the same trade `filePaths.ts` makes for its matching rules.
 */
export function logicalBlockAt(
  getRow: (y: number) => WrappedRow | undefined,
  asked: number,
  cols: number,
): LogicalBlock | null {
  if (cols <= 0 || asked < 0 || !getRow(asked)) {
    return null;
  }
  let startY = asked;
  // A wrapped row continues the one above, so walk back to the row that began it —
  // but only so far. Past the cap the honest answer is the same as for a row whose
  // width cannot be trusted: no links here.
  let back = 0;
  while (startY > 0 && getRow(startY)?.isWrapped) {
    if (++back > MAX_BLOCK_ROWS) {
      return null;
    }
    startY -= 1;
  }
  let text = "";
  for (let y = startY; y - startY <= MAX_BLOCK_ROWS; y += 1) {
    const row = getRow(y);
    if (!row || (y > startY && !row.isWrapped)) {
      return { text, startY };
    }
    if (row.text.length !== cols) {
      return null;
    }
    text += row.text;
  }
  return null;
}

/**
 * Where the character at `offset` in a block sits, in xterm's 1-based cells.
 *
 * Only meaningful for a block {@link logicalBlockAt} returned — its `null` case is
 * precisely when this arithmetic would be wrong.
 */
export function cellOf(offset: number, startY: number, cols: number): LinkCell {
  return {
    x: (offset % cols) + 1,
    y: startY + Math.floor(offset / cols) + 1,
  };
}
