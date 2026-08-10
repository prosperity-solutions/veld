/**
 * Where a dragged tab would land — one drop model for every gesture the dock
 * accepts.
 *
 * Its own module, like `terminalKeys.ts` and `browserError.ts`, because these
 * are pure decisions about coordinates and a false answer from any of them is a
 * user-visible misfire: a preview over the wrong half of the screen, or a window
 * opening on a drag that was merely fumbled. `PaneArea.tsx` holds the React and
 * the DOM; this holds the arithmetic, and the arithmetic has tests.
 */

import type { DockIndex } from "./model";

/**
 * `into` is the dock under the cursor — the tab joins its strip. `left`/`right`
 * are the *pane area's* outer edges and mean "be that side of the split",
 * creating the second dock when there is only one.
 *
 * Reading the edges off the whole area rather than off each dock is what keeps
 * the two cases one rule: with one pane the outer edges are its own, with two
 * they are the outer edges of the pair, and in both the gesture means the same
 * thing.
 *
 * Three members with **unit** discriminants, not two with `"left" | "right"`: a
 * discriminant property has to be a literal type for TypeScript to narrow on it,
 * and a member typed `{where: "left" | "right"}` silently opts the whole union
 * out of that — every read of `.dock` then fails to compile.
 */
export type DropZone = { where: "into"; dock: DockIndex } | { where: "left" } | { where: "right" };

/** A rectangle in client coordinates. `DOMRect` satisfies it structurally, so
 *  tests can pass a plain object and the caller passes the real thing. */
export interface Rect {
  left: number;
  right: number;
  width: number;
}

/**
 * How much of the pane area's width each edge zone takes.
 *
 * Capped in pixels so a wide window does not turn a third of the screen into
 * "split", and floored so a narrow one still has an edge to aim at. The two
 * bounds cross at 175px and 600px of area width; between them it is
 * proportional.
 */
export function edgeWidth(areaWidth: number): number {
  if (!Number.isFinite(areaWidth) || areaWidth <= 0) return 0;
  return Math.max(28, Math.min(96, areaWidth * 0.16));
}

/**
 * Which zone a pointer at `clientX` is in, given the dock it is over.
 *
 * `single` is true when the view shows one pane — the view is not split yet.
 * Then there is nowhere to drop "into" (the whole area *is* the one dock), so
 * the only useful targets are the two splits, decided by the center line
 * rather than the narrow edge zones. That is the discovery behaviour: on an
 * unsplit view a drag into either half visibly promises the split that half
 * will create, instead of a whole-area "into" that hides the feature. With two
 * docks visible the edge zones return, because "into" then means something
 * real (joining the dock under the cursor) and the split is the outlying aim.
 */
export function zoneAt(area: Rect, clientX: number, dock: DockIndex, single: boolean): DropZone {
  if (single) {
    const mid = area.left + area.width / 2;
    return clientX < mid ? { where: "left" } : { where: "right" };
  }
  const edge = edgeWidth(area.width);
  if (clientX <= area.left + edge) return { where: "left" };
  if (clientX >= area.right - edge) return { where: "right" };
  return { where: "into", dock };
}

/** Whether two zones mean the same drop — so `dragover`, which fires per pointer
 *  move, only re-renders when the answer actually changed. */
export function sameZone(a: DropZone | null, b: DropZone | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.where !== b.where) return false;
  return a.where !== "into" || b.where !== "into" || a.dock === b.dock;
}
