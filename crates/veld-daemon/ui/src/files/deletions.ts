/**
 * The "Show deletions" view's arithmetic: where a unified diff's rows sit, in the
 * terms the rest of the file pane speaks — **new-file line numbers**.
 *
 * Everything else in the pane (a reference, the change jumper, `tab.line`) names a
 * line of the file as it is on disk, because that is what an agent can open. A
 * diff adds rows that are not in that file, and its line-selection callback reports
 * them by their *old* line number. So a deleted row is never a line here: a
 * selection is trimmed to the current lines it spans, and one that spans only
 * deleted rows is nothing to copy.
 *
 * Built from `@pierre/diffs`' own parsed hunks rather than the daemon's `-U0`
 * answer, so the stops and rows are the ones the view actually draws.
 *
 * The same arithmetic serves the side-by-side layout (`files.splitDiff`), which
 * differs in one way that matters here: its left column numbers *every* row by the
 * old file, unchanged ones included, so a selection there reports a context line
 * on the deletions side — something a unified diff never does.
 */

import type { FileDiffMetadata, SelectedLineRange } from "@pierre/diffs/react";

import type { LineRange } from "./reference";

/** One run of `-`/`+` lines, located on both sides. */
export type ChangeBlock = {
  /** The new-file line the block's additions start at — or, with none, the line
   *  that follows the deleted rows. */
  newStart: number;
  oldStart: number;
  deletions: number;
  additions: number;
};

/** Every change block in the diff, in file order. */
export function changeBlocks(diff: FileDiffMetadata | null): ChangeBlock[] {
  if (!diff) return [];
  const out: ChangeBlock[] = [];
  for (const hunk of diff.hunks) {
    // A side with no lines is numbered from 0 in a hunk header (`-0,0`), but the
    // next line on it is still line 1.
    let newLine = Math.max(1, hunk.additionStart);
    let oldLine = Math.max(1, hunk.deletionStart);
    for (const part of hunk.hunkContent) {
      if (part.type === "context") {
        newLine += part.lines;
        oldLine += part.lines;
        continue;
      }
      out.push({
        newStart: newLine,
        oldStart: oldLine,
        deletions: part.deletions,
        additions: part.additions,
      });
      newLine += part.additions;
      oldLine += part.deletions;
    }
  }
  return out;
}

/** Where "next change" stops: the first current line of every block, once each,
 *  clamped to the file (a deletion at the end stops on the last line). */
export function diffStops(blocks: ChangeBlock[], lineCount: number): number[] {
  if (lineCount <= 0) return [];
  const stops = blocks.map((b) => Math.min(Math.max(1, b.newStart), lineCount));
  return [...new Set(stops)].sort((a, b) => a - b);
}

/** A row of the diff, named the way its `getLinePosition` takes one: a line
 *  number on one side. */
export type DiffRow = { line: number; side: "deletions" | "additions" };

/**
 * The row a jump to new line `line` should land on. A block that starts there and
 * removed something is shown from its first deleted row — those are drawn
 * **above** what replaced them, so a stop shows what went as well as what came.
 * Anything else is the line's own row. `line` is clamped as a stop is, so a
 * deletion at the end of the file still lands on its deleted rows.
 *
 * Side by side (`split`), a block's first deleted and first added row share one
 * row, so the line's own (right-hand) row is the same place — only a block that
 * added nothing has no right-hand row to land on, and goes by its deleted one.
 */
export function diffRowOf(
  blocks: ChangeBlock[],
  line: number,
  lineCount: number,
  split = false,
): DiffRow {
  const block = blocks.find((b) => Math.min(Math.max(1, b.newStart), lineCount) === line);
  return block && block.deletions > 0 && !(split && block.additions > 0)
    ? { line: block.oldStart, side: "deletions" }
    : { line, side: "additions" };
}

/**
 * The 0-based display row of new line `line` — or, when a block starts there, of
 * that block's first deleted row. Only ever compared with another row, to put a
 * dragged range's ends in order; where a row *is* on screen is the diff's to say.
 */
export function rowOfLine(blocks: ChangeBlock[], line: number): number {
  let deletedAbove = 0;
  for (const b of blocks) {
    if (b.newStart >= line) break;
    deletedAbove += b.deletions;
  }
  return line - 1 + deletedAbove;
}

/** The block a deleted old-file line belongs to. */
function blockOfOld(blocks: ChangeBlock[], old: number): ChangeBlock | undefined {
  return blocks.find((b) => old >= b.oldStart && old < b.oldStart + b.deletions);
}

/** The current line an *unchanged* old-file line is now — shifted by what every
 *  block wholly above it added and removed. */
function newOfOld(blocks: ChangeBlock[], old: number): number {
  let shift = 0;
  for (const b of blocks) {
    if (b.oldStart + b.deletions > old) break;
    shift += b.additions - b.deletions;
  }
  return old + shift;
}

/** An end as the current line it is, when it is one: an unchanged row in the
 *  side-by-side layout's left column is reported by its old number. */
function asCurrent(blocks: ChangeBlock[], end: End): End {
  return end.side === "deletions" && !blockOfOld(blocks, end.line)
    ? { line: newOfOld(blocks, end.line), side: "additions" }
    : end;
}

type End = { line: number; side: "deletions" | "additions" };

/** An end's display row, for putting a dragged range's two ends in order. */
function rowOf(blocks: ChangeBlock[], end: End): number {
  if (end.side === "additions") {
    // Its own block's deletions are above it too.
    let above = 0;
    for (const b of blocks) {
      if (b.newStart > end.line) break;
      above += b.deletions;
    }
    return end.line - 1 + above;
  }
  const block = blockOfOld(blocks, end.line);
  if (!block) return Number.POSITIVE_INFINITY;
  return rowOfLine(blocks, block.newStart) + (end.line - block.oldStart);
}

/**
 * A selection the diff reported, as the current lines it covers — or `null` when it
 * covers only deleted rows. A deleted row at the top of the range moves down to the
 * first current line after it, one at the bottom up to the last before it.
 */
export function newLineRange(
  blocks: ChangeBlock[],
  range: SelectedLineRange,
): LineRange | null {
  const a = asCurrent(blocks, { line: range.start, side: range.side ?? "additions" });
  const b = asCurrent(blocks, {
    line: range.end,
    side: range.endSide ?? range.side ?? "additions",
  });
  const [top, bottom] = rowOf(blocks, a) <= rowOf(blocks, b) ? [a, b] : [b, a];
  const start =
    top.side === "additions" ? top.line : (blockOfOld(blocks, top.line)?.newStart ?? null);
  const end =
    bottom.side === "additions"
      ? bottom.line
      : (() => {
          const block = blockOfOld(blocks, bottom.line);
          return block ? block.newStart - 1 : null;
        })();
  if (start === null || end === null || start > end) return null;
  return { start, end };
}
