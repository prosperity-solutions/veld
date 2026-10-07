/**
 * The text a file pane copies when you select lines and ask for a reference.
 *
 * ```
 * docs/plan.md:12-18
 * > first line of the excerpt
 * > …
 * ```
 *
 * **Shaped for pasting into an agent's prompt**, which is where it goes. The first
 * line is the `path:line` form every coding agent, editor and terminal link
 * matcher already reads; the quote is the excerpt so the agent does not have to go
 * and open the file to know what you meant — and so a reference still says what you
 * were looking at after the file has moved on underneath it. A single line is
 * `path:12`, never `path:12-12`.
 *
 * The path is the daemon's display path: worktree-relative inside the worktree,
 * which is what the agent in that worktree resolves against, and absolute outside
 * it, where a relative path would point at nothing.
 *
 * Always **source** lines, even from the rendered Markdown view: the agent reads
 * the file, not the page, and `**bold**` quoted as "bold" is a line that does not
 * exist in it.
 */

/** A 1-based, inclusive range of source lines. */
export interface LineRange {
  start: number;
  end: number;
}

/** The lines of a text, as a reference counts them — `\r\n` and `\n` alike. */
export function splitLines(text: string): string[] {
  return text.split(/\r?\n/);
}

/** `path:12` or `path:12-18`, with the range put in order. */
export function referenceHeader(path: string, range: LineRange): string {
  const start = Math.min(range.start, range.end);
  const end = Math.max(range.start, range.end);
  return start === end ? `${path}:${start}` : `${path}:${start}-${end}`;
}

/**
 * The whole reference for one range of `lines` (as {@link splitLines} gives them).
 *
 * The range is clamped to the file, so a selection made just before a reload
 * shortened it still copies what is there rather than `undefined`. Each quoted line
 * is `> ` plus the line, with trailing whitespace dropped — so a blank line in the
 * excerpt is a bare `>`, which is how a Markdown quote spells one.
 */
export function formatReference(path: string, lines: string[], range: LineRange): string {
  const last = Math.max(1, lines.length);
  const start = clamp(Math.min(range.start, range.end), 1, last);
  const end = clamp(Math.max(range.start, range.end), 1, last);
  const quoted = lines.slice(start - 1, end).map((line) => `> ${line}`.trimEnd());
  return [referenceHeader(path, { start, end }), ...quoted].join("\n");
}

/**
 * References for a set of lines that need not be contiguous — a sorted CSV table,
 * where the rows you picked can be anywhere in the file.
 *
 * One block per contiguous run, in file order, separated by a blank line. Sorting
 * the table never changes this: the rows carry their source lines, so what you
 * copy points at the file, not at the order you happened to be viewing it in.
 */
export function formatReferences(path: string, lines: string[], ranges: LineRange[]): string {
  return mergeRanges(ranges)
    .map((r) => formatReference(path, lines, r))
    .join("\n\n");
}

/** Sort and merge ranges that touch or overlap. */
export function mergeRanges(ranges: LineRange[]): LineRange[] {
  const sorted = ranges
    .map((r) => ({ start: Math.min(r.start, r.end), end: Math.max(r.start, r.end) }))
    .sort((a, b) => a.start - b.start);
  const out: LineRange[] = [];
  for (const r of sorted) {
    const prev = out[out.length - 1];
    if (prev && r.start <= prev.end + 1) prev.end = Math.max(prev.end, r.end);
    else out.push({ ...r });
  }
  return out;
}

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}
