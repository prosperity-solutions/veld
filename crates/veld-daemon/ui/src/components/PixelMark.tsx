import { createContext, useContext, useEffect, useState } from "react";

import { api } from "../api";
import { cellLit, isBrightColor, pixelPattern } from "../shared/markerPixels";

/**
 * The daemon's glyph allowlist, in its own order — which is what a pixel pattern
 * is an index into.
 *
 * Fetched once per page from `/api/worktree-emoji` rather than kept as a TypeScript
 * copy, for the reason `useMarkerChoices` gives: the list is the server's, and a
 * copy would drift. `null` until it arrives; a pixel marker drawn before then shows
 * every cell unlit, which is a shape no real pattern has, so nothing is ever shown
 * as the wrong worktree while the list loads.
 */
const MarkerOrder = createContext<readonly string[] | null>(null);

export function MarkerOrderProvider(props: { children: React.ReactNode }) {
  const [order, setOrder] = useState<readonly string[] | null>(null);
  useEffect(() => {
    if (order !== null) return;
    let cancelled = false;
    const load = () => {
      void api
        .worktreeEmoji()
        .then((r) => {
          if (!cancelled && Array.isArray(r?.emoji)) setOrder(r.emoji);
        })
        // No toast: the rail still renders every marker, just with its cells
        // unlit. Retried on the next window focus — the same cue `useSettings`
        // retries on — so one failed request (a daemon mid-restart) does not leave
        // every pattern blank until a reload.
        .catch(() => {});
    };
    load();
    window.addEventListener("focus", load);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", load);
    };
  }, [order]);
  return <MarkerOrder.Provider value={order}>{props.children}</MarkerOrder.Provider>;
}

const CELLS = [0, 1, 2].flatMap((row) => [0, 1, 2].map((col) => ({ row, col })));

/**
 * A 3×3 pattern in one colour: lit cells at full strength, unlit ones as a faint
 * tint of the same hue.
 *
 * A tint rather than transparent, so the square's outline is always the same box
 * whatever the pattern — every marker in a column lines up, and a sparse pattern
 * still reads as a marker rather than as two stray dots. The tint is the colour at
 * low opacity rather than a grey, which is what keeps the colour the first thing
 * you see and the shape the second — except for the pale hues on a light panel,
 * which get a darker shade instead (see `isBrightColor`).
 *
 * `className` sizes it. An SVG with a 3-unit viewBox and `crispEdges`, so a cell is
 * a whole number of pixels at every size the stylesheet uses (12px and 18px are both
 * multiples of 3).
 */
export function PixelGrid(props: { color: string; mask: number; className: string }) {
  // Painted with `currentColor`, so the stylesheet can derive the unlit shade from
  // the same value (`.bright` in the light theme) without a second prop.
  const bright = isBrightColor(props.color);
  return (
    <span
      className={`${props.className}${bright ? " bright" : ""}`}
      style={{ color: props.color }}
      aria-hidden
    >
      <svg viewBox="0 0 3 3" shapeRendering="crispEdges" role="presentation">
        {CELLS.map(({ row, col }) => (
          <rect
            key={`${row}${col}`}
            x={col}
            y={row}
            width={1}
            height={1}
            className={cellLit(props.mask, row, col) ? "on" : "off"}
          />
        ))}
      </svg>
    </span>
  );
}

/** A worktree's pixel marker, with its pattern looked up from the glyph. */
export function PixelMark(props: { color: string; emoji: string }) {
  const order = useContext(MarkerOrder);
  const mask = order === null ? 0 : pixelPattern(props.emoji, order);
  return <PixelGrid color={props.color} mask={mask} className="wt-pixels" />;
}
