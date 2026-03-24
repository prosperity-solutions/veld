import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX, FAB_MARGIN, FAB_TOOLBAR_MARGIN } from "./constants";
import { suppressTooltip } from "./tooltip";
import { positionRadialButtons } from "./toolbar";

export function initDrag(): void {
  let startX = 0;
  let startY = 0;
  let origX = 0;
  let origY = 0;
  let dragging = false;
  let moved = false;

  refs.fab.addEventListener("mousedown", function (e: MouseEvent) {
    if (e.button !== 0) return;
    dragging = true;
    moved = false;
    startX = e.clientX;
    startY = e.clientY;
    origX = getState().fabCX;
    origY = getState().fabCY;
    e.preventDefault();
  });

  document.addEventListener("mousemove", function (e: MouseEvent) {
    if (!dragging) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (!moved && Math.abs(dx) < 4 && Math.abs(dy) < 4) return;
    if (!moved) suppressTooltip(true);
    moved = true;
    let nx = origX + dx;
    let ny = origY + dy;
    nx = Math.max(
      20 + FAB_MARGIN,
      Math.min(window.innerWidth - 20 - FAB_MARGIN, nx),
    );
    ny = Math.max(
      20 + FAB_MARGIN,
      Math.min(window.innerHeight - 20 - FAB_MARGIN, ny),
    );
    positionFab(nx, ny, false);
    positionRadialButtons();
  });

  document.addEventListener("mouseup", function () {
    if (!dragging) return;
    dragging = false;
    if (moved) {
      dispatch({ type: "SET_FAB_DRAGGED", dragged: true });
      setTimeout(function () {
        dispatch({ type: "SET_FAB_DRAGGED", dragged: false });
        suppressTooltip(false);
      }, 300);
      saveFabPos(getState().fabCX, getState().fabCY);
    }
  });
}

export function positionFab(cx: number, cy: number, animate: boolean): void {
  dispatch({ type: "SET_FAB_POS", cx, cy });
  refs.toolbarContainer.style.transition = animate ? "all .2s ease" : "none";
  refs.toolbarContainer.style.top = cy - 20 + "px";
  refs.toolbarContainer.style.left = cx - 20 + "px";
}

export function saveFabPos(x: number, y: number): void {
  try {
    sessionStorage.setItem("veld-fab-pos", JSON.stringify({ x: x, y: y }));
  } catch (_) {}
}

export function restoreFabPos(): void {
  try {
    const saved = sessionStorage.getItem("veld-fab-pos");
    if (saved) {
      const pos = JSON.parse(saved);
      positionFab(pos.x, pos.y, false);
      return;
    }
  } catch (_) {}
  positionFab(
    20 + FAB_MARGIN,
    window.innerHeight - 20 - FAB_MARGIN,
    false,
  );
}

export function clampFabToViewport(): void {
  let cx = getState().fabCX;
  let cy = getState().fabCY;
  let clamped = false;
  const maxX = window.innerWidth - 20 - FAB_MARGIN;
  const maxY = window.innerHeight - 20 - FAB_MARGIN;
  const minXY = 20 + FAB_MARGIN;
  if (cx > maxX) {
    cx = maxX;
    clamped = true;
  }
  if (cx < minXY) {
    cx = minXY;
    clamped = true;
  }
  if (cy > maxY) {
    cy = maxY;
    clamped = true;
  }
  if (cy < minXY) {
    cy = minXY;
    clamped = true;
  }
  if (clamped) {
    positionFab(cx, cy, false);
    saveFabPos(cx, cy);
  }
}

/**
 * If the FAB is too close to an edge for the toolbar arc,
 * smoothly animate it inward to the toolbar-safe margin.
 */
export function nudgeFabForToolbar(): void {
  let cx = getState().fabCX;
  let cy = getState().fabCY;
  let nudged = false;
  const maxX = window.innerWidth - 20 - FAB_TOOLBAR_MARGIN;
  const maxY = window.innerHeight - 20 - FAB_TOOLBAR_MARGIN;
  const minXY = 20 + FAB_TOOLBAR_MARGIN;
  if (cx > maxX) { cx = maxX; nudged = true; }
  if (cx < minXY) { cx = minXY; nudged = true; }
  if (cy > maxY) { cy = maxY; nudged = true; }
  if (cy < minXY) { cy = minXY; nudged = true; }
  if (nudged) {
    positionFab(cx, cy, true); // animate: true
    saveFabPos(cx, cy);
  }
}
