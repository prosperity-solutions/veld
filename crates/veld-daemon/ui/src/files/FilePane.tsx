/**
 * The `file` pane: one text file, read-only, drawn by this page.
 *
 * Three renderers, picked by name (`textKind.ts`): Markdown rendered (with a Source
 * view), CSV/TSV as a sortable table (with a Source view), and everything else in
 * the code view — `@pierre/diffs`' `File`, which brings line numbers, highlighting
 * and line selection. Nothing here writes a file; there is no editor, on purpose.
 *
 * # Live, without moving under you
 *
 * While the pane is on screen it re-reads the file every {@link POLL_MS}, with the
 * last `etag`, so an unchanged file costs a 30-byte answer. A changed one is drawn
 * **in place**: the scroll container is the same element before and after, so the
 * position you were reading at survives an agent rewriting the paragraph above it.
 * "On screen" is mounted (only a dock's active tab is) and the page visible — a
 * pane nobody is looking at does not poll, which is the rule `PaneArea` states for
 * every kind.
 *
 * # Copy reference
 *
 * Select lines (code or Source view), a block (rendered Markdown) or rows (table),
 * and the toolbar's Copy reference puts `path:12-18` and the quoted lines on the
 * clipboard — see `reference.ts` for the format and why it is shaped that way.
 *
 * # Show deletions
 *
 * The code and Source views mark this branch's changes in the gutter, and a deleted
 * line is only a red notch there. The toolbar's Show deletions (`files.showDeletions`)
 * swaps in a unified diff of the file's base text (`api.fileBase`, the same base the
 * markers use) against what is on disk, with every unchanged line expanded, so the
 * whole file still reads top to bottom. Line numbers everywhere stay the current
 * file's — see `deletions.ts` for how a deleted row is kept out of a reference.
 * Beside it, Side by side (`files.splitDiff`) lays the same diff out in two
 * columns, base on the left; a reference still cites the right-hand, current lines.
 */

import {
  ActionIcon,
  Button,
  Loader,
  SegmentedControl,
  Text,
  Tooltip,
  useComputedColorScheme,
} from "@mantine/core";
import { parseDiffFromFile } from "@pierre/diffs";
import { File as CodeFile, FileDiff, Virtualizer, useVirtualizer } from "@pierre/diffs/react";
import type { FileDiffMetadata, SelectedLineRange } from "@pierre/diffs/react";
import {
  IconChevronDown,
  IconChevronUp,
  IconClipboardText,
  IconColumns2,
  IconCopy,
  IconExposure,
  IconFileText,
  IconTextWrap,
} from "@tabler/icons-react";
import { useVirtualizer as useRowVirtualizer } from "@tanstack/react-virtual";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, type FileBase, type FileLineChanges, type FileText } from "../api";
import { openExternally } from "../panes/terminalHost";
import { fileLabel, type PaneTab } from "../panes/model";
import { notifyDone, notifyError, notifyRedirect } from "../shared/notify";
import { type CsvTable, parseTable, type SortDir, sortedOrder } from "./csv";
import {
  type ChangeBlock,
  changeBlocks,
  type DiffRow,
  diffRowOf,
  diffStops,
  newLineRange,
} from "./deletions";
import { changeMarkersCss, changeStops } from "./gutter";
import { renderMarkdown, resolveLink } from "./markdown";
import { formatReferences, type LineRange, splitLines } from "./reference";
import { languageFor, textKind } from "./textKind";

/** How often an on-screen pane re-reads its file. */
export const POLL_MS = 1500;

/** What the pane says for each refusal the daemon can give. */
export function refusalText(status: number, message: string): string {
  switch (status) {
    case 404:
      // One status for every refusal, as the file origin does — so this cannot
      // say *which*, and it lists what the user can do about each.
      return "Veld can't show this file. It may not exist, it may be a file Veld never shows (keys, credentials), or it is outside this worktree — open it from this worktree's terminal, or add its folder to files.extraFolders in Settings.";
    // The daemon words these two itself (with the file's actual size), so its
    // sentence wins; the fallbacks cover an older daemon that sent none.
    case 413:
      return message || "This file is too large to show here (over 5 MB). Open it in your editor instead.";
    case 415:
      return message || "This looks like a binary file, so there is no text to show.";
    default:
      return message || `The daemon answered ${status}.`;
  }
}

type Loaded = { file: FileText; lines: string[] };
type View = "rendered" | "table" | "source";

/**
 * A line a view should scroll to, and which request this is.
 *
 * `seq` is what makes the second press of "next change" on the same line, or a
 * re-open at the line already asked for, move the view again: each view remembers
 * the last `seq` it acted on rather than the last line, so a reload of the file
 * (same line, same seq) never yanks the reader back.
 */
type Target = { line: number; seq: number } | null;

/**
 * The source lines a native text selection covers inside `root`, or `[]`.
 *
 * What lets Copy reference work on text selected with the mouse, in every view —
 * each one marks its source lines on the DOM, and this maps both ends of the
 * selection to the nearest mark:
 *
 * - **code / Source**: each row's `data-line`. The rows live in the code view's
 *   shadow root, which `document.getSelection()` does not see into, so the shadow
 *   root's own selection is asked first (Chromium's `ShadowRoot.getSelection`, then
 *   the standard `getComposedRanges`).
 * - **rendered Markdown**: the enclosing block's `data-src-start`/`-end`.
 * - **table**: the row's `data-src-start`/`-end`.
 */
export function rangesFromSelection(root: HTMLElement): LineRange[] {
  const docSel = document.getSelection();
  let range: AbstractRange | null = null;
  const host = root.querySelector("diffs-container");
  const shadow = host?.shadowRoot as
    | (ShadowRoot & { getSelection?: () => Selection | null })
    | null
    | undefined;
  if (shadow) {
    const own = shadow.getSelection?.();
    if (own && !own.isCollapsed && own.rangeCount > 0) range = own.getRangeAt(0);
    else if (docSel && "getComposedRanges" in docSel) {
      const composed = (
        docSel as Selection & {
          getComposedRanges: (o: { shadowRoots: ShadowRoot[] }) => StaticRange[];
        }
      ).getComposedRanges({ shadowRoots: [shadow] })[0];
      if (composed && !composed.collapsed && shadow.contains(composed.startContainer)) {
        range = composed;
      }
    }
  }
  if (!range && docSel && !docSel.isCollapsed && docSel.rangeCount > 0) {
    const r = docSel.getRangeAt(0);
    if (root.contains(r.commonAncestorContainer)) range = r;
  }
  if (!range) return [];
  const a = linesAt(range.startContainer, "next");
  const b = linesAt(range.endContainer, "previous");
  if (!a || !b) return [];
  // A deleted row moved inward (see `linesAt`); ends that crossed mean the
  // selection held deleted rows only, and there is nothing in the file to cite.
  if ((a.moved || b.moved) && a.start > b.end) return [];
  return [{ start: Math.min(a.start, b.start), end: Math.max(a.end, b.end) }];
}

/**
 * The source lines at `node`. A Show deletions diff row that was deleted is not a
 * line of the file — its `data-line` is the *old* number — so it moves to the
 * nearest current row in `toward` (inward from that end of the selection), and says
 * it moved.
 */
function linesAt(
  node: Node,
  toward: "next" | "previous",
): (LineRange & { moved?: boolean }) | null {
  const el = node.nodeType === Node.ELEMENT_NODE ? (node as Element) : node.parentElement;
  if (!el) return null;
  let row = el.closest<HTMLElement>("[data-line]");
  let current = row && currentRow(row);
  let moved = false;
  while (row && !current) {
    moved = true;
    let sib = toward === "next" ? row.nextElementSibling : row.previousElementSibling;
    while (sib && !(sib instanceof HTMLElement && sib.dataset.line)) {
      sib = toward === "next" ? sib.nextElementSibling : sib.previousElementSibling;
    }
    row = sib as HTMLElement | null;
    if (!row) return null;
    current = currentRow(row);
  }
  if (current) {
    const n = Number(current.dataset.line);
    return Number.isFinite(n) && n > 0 ? { start: n, end: n, moved } : null;
  }
  const block = el.closest<HTMLElement>("[data-src-start]");
  if (block) {
    const start = Number(block.dataset.srcStart);
    const end = Number(block.dataset.srcEnd);
    return Number.isFinite(start) && Number.isFinite(end) ? { start, end } : null;
  }
  return null;
}

/**
 * The row that carries `row`'s *current* line number: itself, or `null` for a
 * deleted row. Side by side, the left column numbers even its unchanged rows by
 * the old file, so one there answers with its twin on the right — the same
 * `data-line-index`, which is how the library itself pairs the two columns.
 */
function currentRow(row: HTMLElement): HTMLElement | null {
  if (row.dataset.lineType === "change-deletion") return null;
  const column = row.closest("code[data-deletions]");
  if (!column) return row;
  return (
    column.parentElement?.querySelector<HTMLElement>(
      `code[data-additions] [data-line][data-line-index="${row.dataset.lineIndex}"]`,
    ) ?? null
  );
}

export function FilePane(props: {
  tab: PaneTab;
  worktreeId: number;
  /** Patch this tab — how the pane adopts the daemon's spelling of its path. */
  onTab: (patch: Partial<PaneTab>) => void;
  /** Open another file in a file pane (a relative link in rendered Markdown). */
  onOpenFile: (path: string, line?: number) => void;
  /** `files.wrapLines`, and how the pane's toggle writes it. */
  wrapLines: boolean;
  onWrapLines: (wrap: boolean) => void;
  /** `files.showDeletions`, and how the pane's toggle writes it. */
  showDeletions: boolean;
  onShowDeletions: (show: boolean) => void;
  /** `files.splitDiff`, and how the pane's toggle writes it. */
  splitDiff: boolean;
  onSplitDiff: (split: boolean) => void;
}) {
  const path = props.tab.path ?? "";
  const kind = textKind(path);
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [error, setError] = useState<{ status: number; message: string } | null>(null);
  const [changes, setChanges] = useState<FileLineChanges | null>(null);
  const [base, setBase] = useState<FileBase | null>(null);
  const [view, setView] = useState<View>(
    kind === "markdown" ? "rendered" : kind === "csv" || kind === "tsv" ? "table" : "source",
  );
  /** Whatever is selected in the current view, as source line ranges. */
  const [selection, setSelection] = useState<LineRange[]>([]);
  /** A native text selection inside the pane, mapped to source lines. Preferred
   *  over `selection` when there is one: it is the more recent gesture. */
  const [textSelection, setTextSelection] = useState<LineRange[]>([]);
  // Seeded from `tab.line` by the effect below, on mount as on every later change.
  const [target, setTarget] = useState<Target>(null);
  /** Which change "next"/"previous" last landed on, as an index into the stops. */
  const [changeAt, setChangeAt] = useState(-1);
  // The same index, current the moment it changes: a click steps from here, not
  // from the `changeAt` its handler closed over, so clicks faster than a render
  // still each move one stop on.
  const changeAtRef = useRef(-1);
  const rootRef = useRef<HTMLDivElement>(null);
  const etagRef = useRef<string | null>(null);
  const onTabRef = useRef(props.onTab);
  onTabRef.current = props.onTab;

  // A different file is a different document: nothing about the previous one —
  // its etag, its selection, its markers — applies.
  // biome-ignore lint/correctness/useExhaustiveDependencies: keyed on the file.
  useEffect(() => {
    etagRef.current = null;
    setLoaded(null);
    setError(null);
    setChanges(null);
    setBase(null);
    setSelection([]);
    changeAtRef.current = -1;
    setChangeAt(-1);
  }, [path, props.worktreeId]);

  // A new `line` on the tab is a new request to look there — a re-open, a click.
  const requestedLine = props.tab.line;
  useEffect(() => {
    if (requestedLine) setTarget((t) => ({ line: requestedLine, seq: (t?.seq ?? 0) + 1 }));
  }, [requestedLine]);

  // Follow the native selection. `selectionchange` fires on the document for a
  // selection anywhere, the code view's shadow root included.
  useEffect(() => {
    const update = () => {
      const root = rootRef.current;
      const next = root ? rangesFromSelection(root) : [];
      setTextSelection((prev) =>
        prev.length === next.length &&
        prev.every((r, i) => r.start === next[i].start && r.end === next[i].end)
          ? prev
          : next,
      );
    };
    document.addEventListener("selectionchange", update);
    return () => document.removeEventListener("selectionchange", update);
  }, []);

  // Selections are per view: a block in the rendered view is not a line range in
  // the table, and carrying one across would copy something nobody selected. The
  // diff counts as a view of its own — its highlight is drawn from what it reported.
  // biome-ignore lint/correctness/useExhaustiveDependencies: reset on switch only.
  useEffect(() => setSelection([]), [view, props.showDeletions, props.splitDiff]);

  useEffect(() => {
    if (path === "") return;
    let cancelled = false;
    let timer: number | null = null;
    const read = async () => {
      timer = null;
      if (document.visibilityState === "visible") {
        try {
          const result = await api.fileText(props.worktreeId, path, etagRef.current ?? undefined);
          if (cancelled) return;
          if (result.kind === "text") {
            etagRef.current = result.file.etag;
            setError(null);
            setLoaded({ file: result.file, lines: splitLines(result.file.text) });
            // Adopt the daemon's spelling, so the next open of this file finds this
            // tab rather than opening a second one (`openFileTab`).
            if (result.file.path && result.file.path !== path) {
              onTabRef.current({ path: result.file.path, title: fileLabel(result.file.path) });
            }
          } else if (result.kind === "error") {
            etagRef.current = null;
            setLoaded(null);
            setError({ status: result.status, message: result.message });
          }
        } catch (e) {
          // The daemon is unreachable — a restart, a sleep. The last good render
          // stays; the next tick tries again.
          if (!cancelled && etagRef.current === null) {
            setError({ status: 0, message: e instanceof Error ? e.message : String(e) });
          }
        }
      }
      if (!cancelled) timer = window.setTimeout(() => void read(), POLL_MS);
    };
    void read();
    return () => {
      cancelled = true;
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [path, props.worktreeId]);

  // Gutter markers: re-asked whenever the bytes change, and only for a file inside
  // the worktree — git has nothing to say about one outside it.
  const etag = loaded?.file.etag;
  const inside = loaded?.file.insideWorktree ?? false;
  const shownPath = loaded?.file.path ?? path;
  useEffect(() => {
    if (!etag || !inside) {
      setChanges(null);
      return;
    }
    let cancelled = false;
    api
      .fileLineChanges(props.worktreeId, shownPath)
      .then((c) => !cancelled && setChanges(c))
      .catch(() => !cancelled && setChanges(null));
    return () => {
      cancelled = true;
    };
  }, [etag, inside, shownPath, props.worktreeId]);

  // The base text, for Show deletions: only while it is on and there is a diff to
  // draw it in. Re-asked with the bytes like the markers — the base itself moves
  // only on a rebase, but a file that was untracked becomes tracked on `git add`.
  // The previous answer stays up meanwhile: it is the same text nearly always, and
  // dropping it would flash the plain view on every write.
  const wantsBase = props.showDeletions && inside && view === "source";
  useEffect(() => {
    if (!etag || !wantsBase) return;
    let cancelled = false;
    api
      .fileBase(props.worktreeId, shownPath)
      .then((b) => !cancelled && setBase(b))
      .catch(() => !cancelled && setBase(null));
    return () => {
      cancelled = true;
    };
  }, [etag, wantsBase, shownPath, props.worktreeId]);

  // Nothing to diff against (untracked, added, no commits) or nothing changed: the
  // plain view, which is already the right picture of both.
  const loadedFile = loaded?.file;
  const diff = useMemo<FileDiffMetadata | null>(() => {
    if (!wantsBase || !loadedFile || base?.text == null || base.text === loadedFile.text) {
      return null;
    }
    const lang = languageFor(loadedFile.path);
    return parseDiffFromFile(
      {
        name: loadedFile.path,
        contents: base.text,
        lang,
        cacheKey: `${loadedFile.absPath}@base:${base.base}`,
      },
      {
        name: loadedFile.path,
        contents: loadedFile.text,
        lang,
        cacheKey: `${loadedFile.absPath}@${loadedFile.etag}`,
      },
    );
  }, [wantsBase, loadedFile, base]);
  const blocks = useMemo(() => changeBlocks(diff), [diff]);

  // Changes, as stops for the previous/next buttons — and the first one is where a
  // file picked from Changed files opens (`tab.jump`), once the answer is in. In
  // the diff they are its own blocks, so a stop is a row it actually draws.
  const stops = useMemo(
    () =>
      diff
        ? diffStops(blocks, loaded?.lines.length ?? 0)
        : changeStops(changes, loaded?.lines.length ?? 0),
    [diff, blocks, changes, loaded?.lines.length],
  );
  const goToChange = (at: number) => {
    if (stops.length === 0) return;
    const i = (at + stops.length) % stops.length;
    changeAtRef.current = i;
    setChangeAt(i);
    setTarget((t) => ({ line: stops[i], seq: (t?.seq ?? 0) + 1 }));
  };
  const wantsJump = props.tab.jump === "change";
  // biome-ignore lint/correctness/useExhaustiveDependencies: fires when the answer lands.
  useEffect(() => {
    if (!wantsJump || !loaded) return;
    // Outside the worktree there are no changes to wait for; inside, wait for them.
    if (inside && changes === null) return;
    // Only into a view that shows changes: rendered Markdown and the table draw no
    // markers, so a jump there would land on a line with nothing to say why.
    if (view === "source" && stops.length > 0) goToChange(0);
    onTabRef.current({ jump: undefined });
  }, [wantsJump, loaded, inside, changes, stops]);

  const effective = textSelection.length > 0 ? textSelection : selection;
  const copyReference = () => {
    if (!loaded || effective.length === 0) return;
    const text = formatReferences(loaded.file.path, loaded.lines, effective);
    void navigator.clipboard.writeText(text).then(
      () => notifyDone(`Copied a reference to ${referenceLabel(loaded.file.path, effective)}`),
      (e) => notifyError("Copy the reference", e),
    );
  };

  // The whole file as the daemon read it — the source text, never the rendered
  // Markdown — so a prompt an agent wrote into a `.md` file pastes as written.
  const copyFile = () => {
    if (!loaded) return;
    void navigator.clipboard.writeText(loaded.file.text).then(
      () => notifyDone(`Copied the contents of ${fileLabel(loaded.file.path)}`),
      (e) => notifyError("Copy the file", e),
    );
  };

  // Two spellings per label, and CSS picks one by the bar's own width (a container
  // query on `.file-pane-bar`): a dock at 30% must still show every control whole.
  const label = (long: string, short: string) => (
    <>
      <span className="fp-long">{long}</span>
      <span className="fp-short">{short}</span>
    </>
  );
  const views: { value: View; label: React.ReactNode }[] =
    kind === "markdown"
      ? [
          { value: "rendered", label: label("Rendered", "Doc") },
          { value: "source", label: label("Source", "Src") },
        ]
      : kind === "csv" || kind === "tsv"
        ? [
            { value: "table", label: label("Table", "Table") },
            { value: "source", label: label("Source", "Src") },
          ]
        : [];
  const slash = shownPath.lastIndexOf("/");
  const split = props.showDeletions && props.splitDiff;

  return (
    <div className="file-pane" ref={rootRef}>
      <div className="file-pane-bar">
        <IconFileText size={14} className="file-pane-icon" />
        {/* The one thing allowed to give way. The folder truncates first and the
            file's name last, so a narrow pane still says *which* file; the full
            path is in the tooltip. */}
        <Tooltip label={loaded?.file.absPath ?? path} openDelay={400} withArrow>
          <span className="file-pane-path">
            {slash >= 0 && <span className="file-pane-dir">{shownPath.slice(0, slash + 1)}</span>}
            <span className="file-pane-name">{shownPath.slice(slash + 1)}</span>
          </span>
        </Tooltip>
        {views.length > 0 && (
          <SegmentedControl
            size="xs"
            value={view}
            onChange={(v) => setView(v as View)}
            data={views}
          />
        )}
        <div style={{ flex: 1 }} />
        {/* Only where there is something to step through: a file inside the
            worktree, which git can say something about. Disabled rather than
            hidden when it has no changes, so the bar does not reflow between files. */}
        {inside && view === "source" && (
          // No visible counter (the maintainer asked for arrows only); the position
          // stays on the element so tests and devtools can still read it.
          <div
            className="file-pane-changes"
            data-at={changeAt < 0 ? undefined : `${changeAt + 1}/${stops.length}`}
          >
            <ActionIcon
              size="sm"
              variant="subtle"
              color="gray"
              aria-label="Previous change"
              title={stops.length === 0 ? "No changes" : "Previous change"}
              disabled={stops.length === 0}
              onClick={() => {
                const at = changeAtRef.current;
                goToChange(at <= 0 ? stops.length - 1 : at - 1);
              }}
            >
              <IconChevronUp size={14} />
            </ActionIcon>
            <ActionIcon
              size="sm"
              variant="subtle"
              color="gray"
              aria-label="Next change"
              title={stops.length === 0 ? "No changes" : "Next change"}
              disabled={stops.length === 0}
              onClick={() => goToChange(changeAtRef.current + 1)}
            >
              <IconChevronDown size={14} />
            </ActionIcon>
          </div>
        )}
        {/* Source lines only: rendered Markdown is prose and always wraps, and a
            table's cells clip by design. Writes the setting, so it sticks. */}
        {view === "source" && (
          <Tooltip label={props.wrapLines ? "Stop wrapping long lines" : "Wrap long lines"} withArrow>
            <ActionIcon
              size="sm"
              variant={props.wrapLines ? "light" : "subtle"}
              color="gray"
              aria-label="Wrap long lines"
              aria-pressed={props.wrapLines}
              onClick={() => props.onWrapLines(!props.wrapLines)}
            >
              <IconTextWrap size={14} />
            </ActionIcon>
          </Tooltip>
        )}
        {/* Where the change jumper is: deletions are something only git knows. */}
        {inside && view === "source" && (
          <Tooltip label={props.showDeletions ? "Hide deletions" : "Show deletions"} withArrow>
            <ActionIcon
              size="sm"
              variant={props.showDeletions ? "light" : "subtle"}
              color="gray"
              aria-label="Show deletions"
              aria-pressed={props.showDeletions}
              onClick={() => props.onShowDeletions(!props.showDeletions)}
            >
              <IconExposure size={14} />
            </ActionIcon>
          </Tooltip>
        )}
        {/* Always there, beside Show deletions. Lit only while a split diff is
            what is on screen; pressing it unlit turns the diff on too, so one click
            gets side by side from the plain view (the maintainer's call). Unlit
            again it goes back to inline and leaves the diff on. */}
        {inside && view === "source" && (
          <Tooltip label={split ? "Inline" : "Side by side"} withArrow>
            <ActionIcon
              size="sm"
              variant={split ? "light" : "subtle"}
              color="gray"
              aria-label="Side by side"
              aria-pressed={split}
              onClick={() => {
                if (split) {
                  props.onSplitDiff(false);
                  return;
                }
                if (!props.showDeletions) props.onShowDeletions(true);
                if (!props.splitDiff) props.onSplitDiff(true);
              }}
            >
              <IconColumns2 size={14} />
            </ActionIcon>
          </Tooltip>
        )}
        {/* In every view, beside Copy reference: the other half of handing an
            agent's text on. Icon-only, so the bar's container queries never have
            to hide it at narrow widths. */}
        <Tooltip label="Copy file contents" withArrow>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="gray"
            aria-label="Copy file contents"
            disabled={!loaded || error !== null}
            onClick={copyFile}
          >
            <IconCopy size={14} />
          </ActionIcon>
        </Tooltip>
        <Tooltip
          label={
            effective.length > 0
              ? "Copy path:lines and the quoted lines, to paste into an agent"
              : view === "rendered"
                ? "Select some text, or click a paragraph (shift-click for more)"
                : view === "table"
                  ? "Select some text, or click a row (shift-click for a range)"
                  : diff
                    ? "Select some text, or click a line number — a deleted line is not in the file, so it cannot be cited"
                    : "Select some text, or click a line number (shift-click for a range)"
          }
          openDelay={300}
          withArrow
        >
          <Button
            className="file-pane-copy"
            size="compact-xs"
            variant="default"
            aria-label="Copy reference"
            leftSection={<IconClipboardText size={13} />}
            // Not `disabled`: a disabled button dispatches no pointer events, so
            // the tooltip saying what to do first could never open (#205).
            data-disabled={effective.length === 0 || undefined}
            // Keeps a text selection alive through the click: a mousedown that
            // moved focus to the button would collapse it before `onClick` reads it.
            onMouseDown={(e) => e.preventDefault()}
            onClick={copyReference}
          >
            {label("Copy reference", "Copy")}
          </Button>
        </Tooltip>
      </div>
      {error && (
        <div className="file-pane-message" role="alert">
          <Text size="sm">{refusalText(error.status, error.message)}</Text>
        </div>
      )}
      {!error && !loaded && (
        <div className="file-pane-message">
          <Loader size="sm" />
        </div>
      )}
      {!error && loaded && view === "rendered" && (
        <MarkdownView
          loaded={loaded}
          target={target}
          selection={selection}
          onSelect={setSelection}
          onOpenFile={props.onOpenFile}
        />
      )}
      {!error && loaded && view === "table" && (
        <TableView
          loaded={loaded}
          tab={kind === "tsv"}
          target={target}
          selection={selection}
          onSelect={setSelection}
        />
      )}
      {!error && loaded && view === "source" && diff && (
        <DiffView
          loaded={loaded}
          diff={diff}
          blocks={blocks}
          target={target}
          wrap={props.wrapLines}
          split={props.splitDiff}
          selection={selection}
          onSelect={setSelection}
        />
      )}
      {!error && loaded && view === "source" && !diff && (
        <CodeView
          loaded={loaded}
          changes={changes}
          target={target}
          wrap={props.wrapLines}
          selection={selection}
          onSelect={setSelection}
        />
      )}
    </div>
  );
}

/** `plan.md:12-18`, or `3 ranges in plan.md`, for the confirmation toast. */
function referenceLabel(path: string, ranges: LineRange[]): string {
  if (ranges.length !== 1) return `${ranges.length} ranges in ${fileLabel(path)}`;
  const [r] = ranges;
  return r.start === r.end
    ? `${fileLabel(path)}:${r.start}`
    : `${fileLabel(path)}:${r.start}-${r.end}`;
}

/** Fold a new range into a selection: replace it, or (shift) extend to cover both. */
function extendSelection(prev: LineRange[], next: LineRange, extend: boolean): LineRange[] {
  if (!extend || prev.length === 0) return [next];
  const start = Math.min(next.start, ...prev.map((r) => r.start));
  const end = Math.max(next.end, ...prev.map((r) => r.end));
  return [{ start, end }];
}

// ---------------------------------------------------------------------------
// Code / Source
// ---------------------------------------------------------------------------

function CodeView(props: {
  loaded: Loaded;
  changes: FileLineChanges | null;
  target: Target;
  wrap: boolean;
  selection: LineRange[];
  onSelect: (ranges: LineRange[]) => void;
}) {
  const scheme = useComputedColorScheme("dark");
  const { file, lines } = props.loaded;
  const contents = useMemo(
    () => ({
      name: file.path,
      contents: file.text,
      lang: languageFor(file.path),
      // The highlighter caches by key; the etag changes exactly when the bytes do.
      cacheKey: `${file.absPath}@${file.etag}`,
    }),
    [file.path, file.text, file.absPath, file.etag],
  );
  const markers = useMemo(
    () => changeMarkersCss(props.changes, lines.length),
    [props.changes, lines.length],
  );
  const onSelect = props.onSelect;
  const onLineSelected = useCallback(
    (range: SelectedLineRange | null) =>
      onSelect(range ? [{ start: range.start, end: range.end }] : []),
    [onSelect],
  );
  // Memoised for the same reason as the Markdown body's `inner`: a fresh options
  // object per render is a re-configure per render, for a 1.5 s poll that changed
  // nothing.
  const wrap = props.wrap;
  const options = useMemo(
    () => ({
      theme: { dark: "pierre-dark", light: "pierre-light" },
      themeType: scheme === "light" ? ("light" as const) : ("dark" as const),
      disableFileHeader: true,
      overflow: wrap ? ("wrap" as const) : ("scroll" as const),
      enableLineSelection: true,
      onLineSelected,
      unsafeCSS: markers,
    }),
    [scheme, wrap, onLineSelected, markers],
  );
  const selected = props.selection[0];
  return (
    <Virtualizer className="file-pane-scroll file-code-scroll">
      <CodeFile
        file={contents}
        selectedLines={selected ? { start: selected.start, end: selected.end } : null}
        options={options}
      />
      <RevealLine target={props.target} lineCount={lines.length} />
    </Virtualizer>
  );
}

/**
 * The Show deletions view: the code view's setup — theme, language, wrap, line
 * selection — on a diff with every unchanged line expanded, unified or (`split`)
 * side by side. Its colours are the library's own (red deleted rows, green added
 * ones), so no gutter stylesheet.
 */
function DiffView(props: {
  loaded: Loaded;
  diff: FileDiffMetadata;
  blocks: ChangeBlock[];
  target: Target;
  wrap: boolean;
  split: boolean;
  selection: LineRange[];
  onSelect: (ranges: LineRange[]) => void;
}) {
  const scheme = useComputedColorScheme("dark");
  const { blocks, onSelect } = props;
  // What the diff reported, kept to draw it back: the pane's own selection is in
  // current lines, and a drag that began on a deleted row should stay highlighted
  // from there, not from where the reference starts.
  const [drawn, setDrawn] = useState<SelectedLineRange | null>(null);
  const onLineSelected = useCallback(
    (range: SelectedLineRange | null) => {
      setDrawn(range);
      const lines = range ? newLineRange(blocks, range) : null;
      onSelect(lines ? [lines] : []);
    },
    [blocks, onSelect],
  );
  const { wrap, split } = props;
  // Memoised for the reason `CodeView`'s options are.
  const options = useMemo(
    () => ({
      theme: { dark: "pierre-dark", light: "pierre-light" },
      themeType: scheme === "light" ? ("light" as const) : ("dark" as const),
      disableFileHeader: true,
      overflow: wrap ? ("wrap" as const) : ("scroll" as const),
      diffStyle: split ? ("split" as const) : ("unified" as const),
      // The whole file, not hunks: this is still the file pane, read top to bottom.
      expandUnchanged: true,
      enableLineSelection: true,
      onLineSelected,
    }),
    [scheme, wrap, split, onLineSelected],
  );
  const lineCount = props.loaded.lines.length;
  const rowOf = useCallback(
    (line: number) => diffRowOf(blocks, line, lineCount, split),
    [blocks, lineCount, split],
  );
  return (
    <Virtualizer
      className={`file-pane-scroll file-code-scroll file-diff-scroll${split ? " file-diff-split" : ""}`}
    >
      <FileDiff
        fileDiff={props.diff}
        selectedLines={props.selection.length > 0 ? drawn : null}
        options={options}
      />
      <RevealLine target={props.target} lineCount={lineCount} rowOf={rowOf} split={split} />
    </Virtualizer>
  );
}

/**
 * Where a revealed line goes: this far below the top of the view — two of the
 * library's 20px rows, so a line of what led up to it shows above. Near the top
 * rather than a third of the way down because a jump is a request to *move*: with
 * three changes on screen, a third-down target left the next one where it already
 * was, and the press looked dead (the maintainer's report). Near the top, every
 * press moves unless the scroller has run out of room.
 */
const REVEAL_MARGIN = 40;
/** Off by less than this, a line is where it was sent: not worth a visible nudge. */
const REVEAL_SLOP = 4;
/** Nudges allowed after the first jump, and frames to watch for them. */
const REVEAL_CORRECTIONS = 3;
const REVEAL_SETTLE_FRAMES = 12;
/** Frames to wait for the code view to be there to measure before giving up. */
const REVEAL_WAIT_FRAMES = 60;

/**
 * The scroll position that puts a row starting `lineTop` into the content
 * `REVEAL_MARGIN` below the top of a `viewport`-high view — clamped to what the
 * scroller can reach, so a line near either end is not chased toward a position it
 * can never take. (That is why a landing is also flashed: in the last screenful
 * the scroll cannot move, and the flash is what says the press did anything.)
 */
export function revealScrollTop(lineTop: number, viewport: number, scrollHeight: number): number {
  const max = Math.max(0, scrollHeight - viewport);
  return Math.round(Math.min(max, Math.max(0, lineTop - REVEAL_MARGIN)));
}

/** How long a landed-on row is lit. */
export const FLASH_MS = 600;
/** The flash: an inset shadow rather than a background, so it lies over the
 *  row's own red or green and fades out to exactly what was there. */
const FLASH_ON = "inset 0 0 0 100vmax rgba(250, 176, 5, 0.35)";
const FLASH_OFF = "inset 0 0 0 100vmax rgba(250, 176, 5, 0)";

/**
 * Light up the display row `row` sits on — its line number and its text, and side
 * by side the other column's half of the same row — for {@link FLASH_MS}.
 *
 * Web Animations on the elements themselves rather than a class and a stylesheet:
 * the rows live in the code view's shadow root, out of reach of this page's CSS,
 * and an animation leaves nothing behind for the library's next render to trip
 * over. Under `prefers-reduced-motion` the light holds still and then goes,
 * without the fade.
 */
function flashRow(row: Element, split: boolean): void {
  const shadow = row.getRootNode();
  const index = row.getAttribute("data-line-index");
  if (!(shadow instanceof ShadowRoot) || index === null) return;
  // `unified,split` on a diff's rows, a plain index on a file's. Side by side the
  // two columns share only the second half: their unified halves differ.
  const selector = split
    ? `[data-line-index$=",${index.split(",")[1]}"]`
    : `[data-line-index="${index}"]`;
  const still = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  for (const el of shadow.querySelectorAll(selector)) {
    if (typeof el.animate !== "function") continue; // jsdom
    el.animate(
      still
        ? [{ boxShadow: FLASH_ON }, { boxShadow: FLASH_ON }]
        : [{ boxShadow: FLASH_ON }, { boxShadow: FLASH_OFF }],
      { duration: FLASH_MS, easing: "ease-in" },
    );
  }
}

type LinePositions = {
  getLinePosition(
    line: number,
    side?: DiffRow["side"],
  ): { top: number; height: number } | undefined;
};

/**
 * The `File`/`FileDiff` instance drawing into `host`. `@pierre/diffs` keeps it
 * inside its React wrapper — no ref, and the hook that makes it hands back only a
 * callback ref — so the virtualizer's registry of what it drives is the one place
 * it is reachable. Private in the types, a `Map` at runtime: checked rather than
 * trusted, so an upgrade that moves it loses the layout of undrawn rows (a jump
 * then lands once the row is drawn) instead of throwing, and a test trips first.
 */
export function linePositionsOf(virtualizer: unknown, host: Element): LinePositions | undefined {
  const registry = (virtualizer as { observers?: unknown } | undefined)?.observers;
  if (!(registry instanceof Map)) return undefined;
  const instance: unknown = registry.get(host);
  return typeof (instance as Partial<LinePositions> | undefined)?.getLinePosition === "function"
    ? (instance as LinePositions)
    : undefined;
}

/** A diff row's element. `data-line` is the row's own side's number, so a deleted
 *  row and the current line that shares its number tell apart by type — or, side by
 *  side, by column: there the left one numbers every row by the old file. */
function rowSelector(row: DiffRow, split: boolean): string {
  if (split) return `code[data-${row.side}] [data-line="${row.line}"]`;
  const deleted = '[data-line-type="change-deletion"]';
  return `[data-line="${row.line}"]${row.side === "deletions" ? deleted : `:not(${deleted})`}`;
}

/**
 * Where `row` starts in the scroller's content, and whether that is the drawn row
 * itself or the library's layout of one it has not drawn yet — measured heights
 * where it has them, its per-row estimate where it does not. Both from live
 * rects, not the instance's cached `top`, which is only as fresh as its last
 * render pass.
 */
function locateRow(
  virtualizer: unknown,
  root: HTMLElement,
  row: DiffRow,
  split: boolean,
): { top: number; drawn?: Element } | undefined {
  const host = root.querySelector("diffs-container");
  if (!host) return undefined;
  const origin = root.getBoundingClientRect().top - root.scrollTop;
  const drawn = host.shadowRoot?.querySelector(rowSelector(row, split));
  if (drawn) return { top: drawn.getBoundingClientRect().top - origin, drawn };
  const pos = linePositionsOf(virtualizer, host)?.getLinePosition(row.line, row.side);
  if (!pos) return undefined;
  // Local to the file's element, which sits wherever the scroller has put it.
  return { top: host.getBoundingClientRect().top - origin + pos.top };
}

const plainRow = (line: number): DiffRow => ({ line, side: "additions" });

/** What the reader does to take over the scroll; any of it ends a reveal. */
const HANDS_ON = ["wheel", "touchstart", "pointerdown", "keydown"] as const;

/**
 * Scroll the code view to the line the pane was asked for — once per request, not
 * on every reload (that would yank the reader back while they scroll).
 *
 * One jump to where the library lays the row out, then a few frames of watching:
 * wrapped rows it has not drawn are estimates, and drawing them can move the
 * target. Each frame re-measures — the drawn row once it exists — and nudges only
 * when the line is off by more than `REVEAL_SLOP`, so the usual case is a single
 * jump — made even when the line is already on screen, since a press is a request
 * to bring it up top. Only the code view's own scroller moves, never the pane
 * around it. Once settled (or out of frames) the landed-on row flashes; a newer
 * request, or the reader taking the wheel, ends the watch without one.
 */
function RevealLine(props: {
  target: Target;
  lineCount: number;
  /** The row a line lands on, when rows are not lines (the deletions diff). */
  rowOf?: (line: number) => DiffRow;
  /** The diff is side by side, which changes how a drawn row is found. */
  split?: boolean;
}) {
  const virtualizer = useVirtualizer();
  const done = useRef<number | null>(null);
  // Read at jump time, not a dependency: a reload that rebuilds it mid-watch
  // must not cancel the jump in flight.
  const rowOfRef = useRef(props.rowOf);
  rowOfRef.current = props.rowOf;
  const { lineCount } = props;
  const split = props.split ?? false;
  const line = props.target?.line;
  const seq = props.target?.seq;
  useEffect(() => {
    if (!virtualizer || !line || !seq || lineCount === 0 || done.current === seq) return;
    done.current = seq;
    const row = (rowOfRef.current ?? plainRow)(Math.min(line, lineCount));
    let frame = 0;
    let jumps = 0;
    let budget = REVEAL_WAIT_FRAMES;
    let raf = 0;
    let scroller: HTMLElement | null = null;
    const stop = () => {
      window.cancelAnimationFrame(raf);
      raf = 0;
      for (const type of HANDS_ON) scroller?.removeEventListener(type, stop);
    };
    const tick = () => {
      frame += 1;
      const root = virtualizer.getRoot();
      if (root instanceof HTMLElement && !scroller) {
        scroller = root;
        for (const type of HANDS_ON) root.addEventListener(type, stop, { passive: true });
      }
      const at = root instanceof HTMLElement ? locateRow(virtualizer, root, row, split) : undefined;
      // Measured this frame, so it is the row as drawn now — never one the library
      // has since replaced or recycled for another line. Undrawn, nothing to light.
      const land = () => {
        stop();
        if (at?.drawn) flashRow(at.drawn, split);
      };
      if (root instanceof HTMLElement && at) {
        const top = revealScrollTop(at.top, root.clientHeight, root.scrollHeight);
        if (Math.abs(top - root.scrollTop) > REVEAL_SLOP) {
          if (jumps > REVEAL_CORRECTIONS) return land();
          if (jumps === 0) budget = frame + REVEAL_SETTLE_FRAMES;
          jumps += 1;
          virtualizer.scrollTo({ top, behavior: "instant" });
        } else if (at.drawn) {
          return land(); // Drawn, and where it was sent: settled.
        }
      }
      if (frame >= budget) return land();
      raf = window.requestAnimationFrame(tick);
    };
    tick();
    return () => {
      // Torn down mid-watch (a re-render's dependency change, StrictMode's
      // double effect): the request was not carried out, so the next run may.
      if (raf !== 0) done.current = null;
      stop();
    };
  }, [virtualizer, line, seq, lineCount, split]);
  return null;
}

// ---------------------------------------------------------------------------
// Rendered Markdown
// ---------------------------------------------------------------------------

function MarkdownView(props: {
  loaded: Loaded;
  target: Target;
  selection: LineRange[];
  onSelect: (ranges: LineRange[]) => void;
  onOpenFile: (path: string, line?: number) => void;
}) {
  const { file } = props.loaded;
  const html = useMemo(() => renderMarkdown(file.text), [file.text]);
  // The *object* memoised as well as the string: React compares this prop by
  // identity, so a fresh `{ __html }` per render re-set `innerHTML` on every
  // render — a click selecting a block, a poll — wiping a text selection and the
  // DOM it was in. Same bytes, same object, same nodes.
  const inner = useMemo(() => ({ __html: html }), [html]);
  const body = useRef<HTMLDivElement>(null);
  const revealed = useRef<number | null>(null);

  // Mark the selected blocks. Re-run on `html` too: a reload replaces the DOM.
  useEffect(() => {
    const el = body.current;
    if (!el) return;
    const sel = props.selection[0];
    for (const block of el.querySelectorAll<HTMLElement>("[data-src-start]")) {
      const start = Number(block.dataset.srcStart);
      const end = Number(block.dataset.srcEnd);
      block.classList.toggle("md-selected", !!sel && start >= sel.start && end <= sel.end);
    }
  }, [props.selection, html]);

  useEffect(() => {
    const el = body.current;
    const line = props.target?.line;
    const seq = props.target?.seq;
    if (!el || !line || !seq || revealed.current === seq) return;
    revealed.current = seq;
    // The innermost block covering the line: the last one in document order,
    // since a block's children follow it.
    let hit: HTMLElement | null = null;
    for (const block of el.querySelectorAll<HTMLElement>("[data-src-start]")) {
      if (Number(block.dataset.srcStart) <= line && Number(block.dataset.srcEnd) >= line) {
        hit = block;
      }
    }
    hit?.scrollIntoView({ block: "center" });
  }, [props.target, html]);

  const onClick = (e: React.MouseEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    const anchor = target.closest("a");
    if (anchor) {
      // Never navigate this page: it is the IDE. Every link goes somewhere else.
      e.preventDefault();
      const to = resolveLink(file.path, anchor.getAttribute("href") ?? "");
      if (to.kind === "external") openExternally(to.url);
      else if (to.kind === "file") {
        const k = textKind(to.path);
        if (k === "plain" && !/\.(txt|log)$/i.test(to.path)) {
          notifyRedirect(`${to.path} is not a text file a file pane can show`);
        } else {
          props.onOpenFile(to.path, to.line);
        }
      }
      return;
    }
    // A drag that selected text is someone copying words, not picking a block.
    if (!window.getSelection()?.isCollapsed) return;
    const block = target.closest<HTMLElement>("[data-src-start]");
    if (!block || !body.current?.contains(block)) return;
    const range = { start: Number(block.dataset.srcStart), end: Number(block.dataset.srcEnd) };
    props.onSelect(extendSelection(props.selection, range, e.shiftKey));
  };

  return (
    <div className="file-pane-scroll md-scroll">
      {/* A block is picked by clicking it; the keyboard route to a reference is
          the Source view's line selection, which is the same lines. */}
      {/* biome-ignore lint/a11y/useKeyWithClickEvents: see above. */}
      {/* biome-ignore lint/a11y/noStaticElementInteractions: see above. */}
      <div
        ref={body}
        className="md-body"
        onClick={onClick}
        // Sanitized in `renderMarkdown` (markdown-it with `html: false`, then
        // DOMPurify) — see that module for why this is safe on this origin.
        // biome-ignore lint/security/noDangerouslySetInnerHtml: sanitized above.
        dangerouslySetInnerHTML={inner}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// CSV / TSV table
// ---------------------------------------------------------------------------

const ROW_HEIGHT = 26;

/** A column's width from its widest early cell — measured once, not per render. */
function columnWidths(table: CsvTable): number[] {
  const widths: number[] = [];
  const sample = table.rows.slice(0, 200);
  for (let c = 0; c < table.columns; c++) {
    let chars = (table.header[c] ?? "").length + 2;
    for (const row of sample) chars = Math.max(chars, (row.cells[c] ?? "").length);
    widths.push(Math.min(360, Math.max(64, chars * 7.5 + 16)));
  }
  return widths;
}

function TableView(props: {
  loaded: Loaded;
  tab: boolean;
  target: Target;
  selection: LineRange[];
  onSelect: (ranges: LineRange[]) => void;
}) {
  const { file } = props.loaded;
  const table = useMemo(() => parseTable(file.text, props.tab), [file.text, props.tab]);
  const widths = useMemo(() => columnWidths(table), [table]);
  const [sort, setSort] = useState<{ column: number; dir: SortDir } | null>(null);
  const order = useMemo(() => sortedOrder(table.rows, sort), [table, sort]);
  const scroller = useRef<HTMLDivElement>(null);
  const anchor = useRef<number | null>(null);
  const virtual = useRowVirtualizer({
    count: order.length,
    getScrollElement: () => scroller.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });
  const template = `56px ${widths.map((w) => `${w}px`).join(" ")}`;
  const totalWidth = 56 + widths.reduce((a, b) => a + b, 0);

  const revealed = useRef<number | null>(null);
  useEffect(() => {
    const line = props.target?.line;
    const seq = props.target?.seq;
    if (!line || !seq || revealed.current === seq) return;
    revealed.current = seq;
    const at = order.findIndex((i) => table.rows[i].startLine <= line && table.rows[i].endLine >= line);
    if (at >= 0) virtual.scrollToIndex(at, { align: "center" });
  }, [props.target, order, table, virtual]);

  const isSelected = (rowIndex: number) => {
    const row = table.rows[rowIndex];
    return props.selection.some((r) => row.startLine >= r.start && row.endLine <= r.end);
  };

  const clickRow = (e: React.MouseEvent, position: number) => {
    const row = table.rows[order[position]];
    const own = { start: row.startLine, end: row.endLine };
    if (e.shiftKey && anchor.current !== null) {
      // A shift-click is a range *as displayed* — under a sort those rows can be
      // anywhere in the file, so the selection is one range per row, merged into
      // runs only where the file has them adjacent.
      const [a, b] = [anchor.current, position].sort((x, y) => x - y);
      const ranges = order
        .slice(a, b + 1)
        .map((i) => ({ start: table.rows[i].startLine, end: table.rows[i].endLine }));
      props.onSelect(ranges);
      return;
    }
    anchor.current = position;
    if (e.metaKey || e.ctrlKey) {
      const has = props.selection.some((r) => r.start === own.start && r.end === own.end);
      props.onSelect(
        has
          ? props.selection.filter((r) => !(r.start === own.start && r.end === own.end))
          : [...props.selection, own],
      );
      return;
    }
    props.onSelect([own]);
  };

  const cycleSort = (column: number) =>
    setSort((s) =>
      s?.column !== column
        ? { column, dir: "asc" }
        : s.dir === "asc"
          ? { column, dir: "desc" }
          : null,
    );

  return (
    <div className="file-pane-scroll csv-scroll" ref={scroller}>
      {table.errors > 0 && (
        <div className="csv-note">
          Some rows did not parse cleanly ({table.errors}); they are shown as read.
        </div>
      )}
      <div className="csv-grid" style={{ width: totalWidth }}>
        <div className="csv-row csv-head" style={{ gridTemplateColumns: template }}>
          <span className="csv-line">line</span>
          {Array.from({ length: table.columns }, (_, c) => (
            <button
              type="button"
              // biome-ignore lint/suspicious/noArrayIndexKey: columns are positional.
              key={c}
              className="csv-cell csv-sort"
              onClick={() => cycleSort(c)}
              aria-sort={
                sort?.column === c ? (sort.dir === "asc" ? "ascending" : "descending") : "none"
              }
            >
              {table.header[c] ?? ""}
              {sort?.column === c ? (sort.dir === "asc" ? " ▲" : " ▼") : ""}
            </button>
          ))}
        </div>
        <div style={{ height: virtual.getTotalSize(), position: "relative" }}>
          {virtual.getVirtualItems().map((item) => {
            const rowIndex = order[item.index];
            const row = table.rows[rowIndex];
            return (
              // A row is a click target for selection, and the keyboard path to the
              // same thing is the Source view's line selection — a table of
              // focusable rows would put every row of a 50 000-line file in the tab
              // order.
              // biome-ignore lint/a11y/useKeyWithClickEvents: see above.
              <div
                key={rowIndex}
                className={`csv-row${isSelected(rowIndex) ? " selected" : ""}`}
                // Read by `rangesFromSelection`, so text selected across rows maps
                // to the rows' own source lines, whatever the sort.
                data-src-start={row.startLine}
                data-src-end={row.endLine}
                style={{
                  gridTemplateColumns: template,
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: totalWidth,
                  height: ROW_HEIGHT,
                  transform: `translateY(${item.start}px)`,
                }}
                onClick={(e) => clickRow(e, item.index)}
              >
                <span className="csv-line">{row.startLine}</span>
                {Array.from({ length: table.columns }, (_, c) => (
                  // biome-ignore lint/suspicious/noArrayIndexKey: columns are positional.
                  <span key={c} className="csv-cell" title={row.cells[c] ?? ""}>
                    {row.cells[c] ?? ""}
                  </span>
                ))}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
