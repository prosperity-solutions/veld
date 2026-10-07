/**
 * A CSV or TSV file as a table, with every row remembering where it came from.
 *
 * **Every row, no cap.** A table that silently stops at row 10 000 answers "is this
 * value in here?" wrongly, and the file is already bounded by the daemon's size cap;
 * the table is virtualized, so the DOM cost is the rows on screen, not the file.
 *
 * **A row's lines are the reason this is not just `Papa.parse(text).data`.** A
 * reference copied from a table has to name the file's lines, and the table can be
 * sorted — so each row carries the 1-based line it starts on and the one it ends on
 * (a quoted field can hold a newline, so those differ), and sorting moves rows
 * without touching either. papaparse's `step` reports the cursor after each row,
 * which is what the lines are counted from.
 */

import Papa from "papaparse";

export interface CsvRow {
  cells: string[];
  /** 1-based, inclusive. */
  startLine: number;
  endLine: number;
}

export interface CsvTable {
  /** The first row, which is taken to be the header. */
  header: string[];
  rows: CsvRow[];
  /** The widest row's cell count — a ragged file still gets a column per cell. */
  columns: number;
  /** papaparse's complaints, for a one-line note above the table. */
  errors: number;
}

/** Parse `text` as CSV, or TSV when `tab` is set. */
export function parseTable(text: string, tab: boolean): CsvTable {
  const rows: CsvRow[] = [];
  let errors = 0;
  // Running newline count up to `consumed`, so each row's lines cost the distance
  // from the previous cursor rather than a rescan from the start.
  let consumed = 0;
  let line = 1;
  const advance = (to: number) => {
    for (let i = consumed; i < to && i < text.length; i++) {
      if (text.charCodeAt(i) === 10) line++;
    }
    consumed = Math.max(consumed, to);
  };
  Papa.parse<string[]>(text, {
    delimiter: tab ? "\t" : "",
    skipEmptyLines: "greedy",
    step: (result) => {
      if (result.errors.length > 0) errors += result.errors.length;
      const cursor = result.meta.cursor;
      // Skip the newlines between the previous row and this one (blank lines that
      // `skipEmptyLines` dropped) by starting from the first non-newline character.
      let start = consumed;
      while (start < text.length && (text[start] === "\n" || text[start] === "\r")) start++;
      advance(start);
      const startLine = line;
      // The cursor sits after the row's own line break, if it had one; the row ends
      // on the line before that break.
      let end = cursor;
      while (end > start && (text[end - 1] === "\n" || text[end - 1] === "\r")) end--;
      advance(end);
      const endLine = line;
      advance(cursor);
      rows.push({ cells: result.data, startLine, endLine });
    },
  });
  const header = rows.shift()?.cells ?? [];
  const columns = rows.reduce((n, r) => Math.max(n, r.cells.length), header.length);
  return { header, rows, columns, errors };
}

export type SortDir = "asc" | "desc";

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/**
 * The rows in display order for a sort, as indices into `table.rows` — never a
 * copy of the rows, so a selection keyed by index survives a re-sort.
 *
 * Numeric-aware (`2` before `10`), and stable: ties keep file order, which is the
 * order the person last saw them in when the sort was off.
 */
export function sortedOrder(
  rows: CsvRow[],
  sort: { column: number; dir: SortDir } | null,
): number[] {
  const order = rows.map((_, i) => i);
  if (!sort) return order;
  const sign = sort.dir === "asc" ? 1 : -1;
  return order.sort((a, b) => {
    const c = collator.compare(rows[a].cells[sort.column] ?? "", rows[b].cells[sort.column] ?? "");
    return c !== 0 ? sign * c : a - b;
  });
}
