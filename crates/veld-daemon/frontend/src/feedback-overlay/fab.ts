// FAB (bubble) positioning + persistence.
//
// Drag, momentum, bounce, and live viewport collision are all owned by the
// arc-menu engine now. This module keeps the position API the rest of the
// overlay relies on (restore/clamp on init, nudge for the arc) and mirrors the
// bubble center into the store + sessionStorage.
import { getState, dispatch } from "./store";
import { FAB_MARGIN } from "./constants";
import { getArc } from "./toolbar";

/** Move the bubble center to (cx, cy). Updates the engine + store. */
export function positionFab(cx: number, cy: number, animate: boolean): void {
  dispatch({ type: "SET_FAB_POS", cx, cy });
  getArc()?.setPosition(cx, cy, animate);
}

export function saveFabPos(x: number, y: number): void {
  try {
    sessionStorage.setItem("veld-fab-pos", JSON.stringify({ x, y }));
  } catch (_) { /* ignore */ }
}

export function restoreFabPos(): void {
  try {
    const saved = sessionStorage.getItem("veld-fab-pos");
    if (saved) {
      const pos = JSON.parse(saved);
      positionFab(pos.x, pos.y, false);
      return;
    }
  } catch (_) { /* ignore */ }
  positionFab(20 + FAB_MARGIN, window.innerHeight - 20 - FAB_MARGIN, false);
}

/** Clamp the bubble into the viewport (called on init + resize). */
export function clampFabToViewport(): void {
  const arc = getArc();
  if (arc) {
    arc.clampToViewport();
    return;
  }
  // Fallback (no engine): simple viewport clamp against the store position.
  const maxX = window.innerWidth - 20 - FAB_MARGIN;
  const maxY = window.innerHeight - 20 - FAB_MARGIN;
  const minXY = 20 + FAB_MARGIN;
  const cx = Math.max(minXY, Math.min(maxX, getState().fabCX));
  const cy = Math.max(minXY, Math.min(maxY, getState().fabCY));
  if (cx !== getState().fabCX || cy !== getState().fabCY) {
    positionFab(cx, cy, false);
    saveFabPos(cx, cy);
  }
}
