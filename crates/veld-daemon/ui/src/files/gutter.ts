/**
 * Changed-line markers for the code view's gutter, as CSS.
 *
 * CSS rather than per-line annotations because the code view renders inside a
 * shadow root it owns (`@pierre/diffs`), and the one supported way in is its
 * `unsafeCSS` option. Each gutter cell carries `data-column-number="<line>"`, so a
 * marker is a selector — and it keeps working for lines the virtualizer has not
 * rendered yet, because a rule needs no element to exist before it applies.
 *
 * "unsafe" is the library's name for "unvalidated"; everything interpolated here is
 * an integer this module produced, never text from the file or the daemon.
 *
 * The colours are the gutter's conventional three — green added, blue modified, a
 * red notch for a deletion between two lines. Custom properties inherit into a
 * shadow root, so `--veld-gutter-*` on the pane can retune them; the fallbacks are
 * the session palette's green, blue and red, which read on both themes.
 */

import type { FileLineChanges } from "../api";

/**
 * The lines one list of ranges covers, or `"all"` when it is the whole file — an
 * untracked file is all added, and ten thousand selectors for that is a stylesheet
 * the size of the file.
 */
function rangeLines(ranges: [number, number][], lineCount: number): number[] | "all" {
  const out: number[] = [];
  for (const [rawStart, rawEnd] of ranges) {
    const start = Math.max(1, Math.trunc(Math.min(rawStart, rawEnd)));
    const end = Math.min(lineCount, Math.trunc(Math.max(rawStart, rawEnd)));
    if (!Number.isFinite(start) || !Number.isFinite(end)) continue;
    if (start <= 1 && end >= lineCount) return "all";
    for (let line = start; line <= end; line++) out.push(line);
  }
  return out;
}

/** The gutter cell for each line, and the code row for each line. Gutter cells
 *  carry `data-column-number`; code rows carry `data-line` and nothing in the
 *  gutter does, so the two never select each other. */
function gutterCells(lines: number[] | "all"): string {
  return lines === "all"
    ? "[data-gutter] [data-column-number]"
    : lines.map((n) => `[data-gutter] [data-column-number="${n}"]`).join(",");
}
function codeRows(lines: number[] | "all"): string {
  return lines === "all" ? "[data-line]" : lines.map((n) => `[data-line="${n}"]`).join(",");
}

/**
 * The row tint, GitHub-style: a soft wash over the whole line, so a change reads at
 * a glance and not only from the 3px gutter bar. Low-alpha versions of the bar's own
 * colours, which sit on both themes. A background *image* rather than a colour,
 * because the code view paints a selected line with `background-color`, and an
 * image layers over it instead of replacing it — a changed line can still be
 * selected and show it. Per row, so it covers a wrapped line's full height.
 */
function tint(color: string): string {
  return `background-image:linear-gradient(${color},${color});`;
}

/**
 * A 2px red edge, drawn as a background rather than a border: a border would make
 * this one gutter cell 2px taller than its code row and walk every line below it
 * out of alignment. A background also composes with the added/modified bar, which
 * is a box-shadow — a line can be both modified and just below a deletion.
 */
function notch(edge: "top" | "bottom"): string {
  return `background-image:linear-gradient(var(--veld-gutter-deleted,#e05a50),var(--veld-gutter-deleted,#e05a50));background-size:100% 2px;background-repeat:no-repeat;background-position:${edge};`;
}

/** The stylesheet for a file's changes, or `""` when there are none. */
export function changeMarkersCss(changes: FileLineChanges | null, lineCount: number): string {
  if (!changes || lineCount <= 0) return "";
  const rules: string[] = [];
  const added = rangeLines(changes.added ?? [], lineCount);
  const modified = rangeLines(changes.modified ?? [], lineCount);
  if (added === "all" || added.length > 0) {
    rules.push(`${gutterCells(added)}{box-shadow:inset 3px 0 0 var(--veld-gutter-added,#3fbf7f);}`);
    rules.push(`${codeRows(added)}{${tint("var(--veld-line-added,rgba(63,191,127,0.13))")}}`);
  }
  if (modified === "all" || modified.length > 0) {
    rules.push(
      `${gutterCells(modified)}{box-shadow:inset 3px 0 0 var(--veld-gutter-modified,#5aa2e0);}`,
    );
    rules.push(`${codeRows(modified)}{${tint("var(--veld-line-modified,rgba(90,162,224,0.13))")}}`);
  }
  // `deleted` is git's `+N,0`: the lines went *after* line N, so the notch is on
  // N's bottom edge — and on line 1's top edge for N = 0, a deletion above it.
  const deleted = (changes.deleted ?? [])
    .map((n) => Math.trunc(n))
    .filter((n) => Number.isFinite(n) && n >= 0)
    .map((n) => Math.min(n, lineCount));
  const below = deleted.filter((n) => n >= 1);
  if (below.length > 0) {
    rules.push(
      `${below.map((n) => `[data-gutter] [data-column-number="${n}"]`).join(",")}{${notch("bottom")}}`,
    );
  }
  if (deleted.includes(0)) {
    rules.push(`[data-gutter] [data-column-number="1"]{${notch("top")}}`);
  }
  return rules.join("\n");
}

/**
 * Where the changes are, as the lines a "next change" button stops at: the first
 * line of every added or modified range and every deletion, in file order, once
 * each. A deletion stops on the line it follows (line 1 for one above it), where
 * its notch is drawn.
 */
export function changeStops(changes: FileLineChanges | null, lineCount: number): number[] {
  if (!changes || lineCount <= 0) return [];
  const starts = [
    ...(changes.added ?? []).map(([a, b]) => Math.min(a, b)),
    ...(changes.modified ?? []).map(([a, b]) => Math.min(a, b)),
    ...(changes.deleted ?? []),
  ]
    .map((n) => Math.min(Math.max(1, Math.trunc(n)), lineCount))
    .filter((n) => Number.isFinite(n));
  return [...new Set(starts)].sort((a, b) => a - b);
}
