import { S } from "./state";
import { PREFIX, FAB_MARGIN } from "./constants";

export function initDrag(): void {
  let startX = 0;
  let startY = 0;
  let origX = 0;
  let origY = 0;
  let dragging = false;
  let moved = false;

  S.fab.addEventListener("mousedown", function (e: MouseEvent) {
    if (e.button !== 0) return;
    dragging = true;
    moved = false;
    startX = e.clientX;
    startY = e.clientY;
    origX = S.fabCX;
    origY = S.fabCY;
    e.preventDefault();
  });

  document.addEventListener("mousemove", function (e: MouseEvent) {
    if (!dragging) return;
    const dx = e.clientX - startX;
    const dy = e.clientY - startY;
    if (!moved && Math.abs(dx) < 4 && Math.abs(dy) < 4) return;
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
  });

  document.addEventListener("mouseup", function () {
    if (!dragging) return;
    dragging = false;
    if (moved) {
      (S.fab as any)._wasDragged = true;
      setTimeout(function () {
        (S.fab as any)._wasDragged = false;
      }, 300);
      saveFabPos(S.fabCX, S.fabCY);
    }
  });
}

export function positionFab(cx: number, cy: number, animate: boolean): void {
  S.fabCX = cx;
  S.fabCY = cy;
  const onRight = cx > window.innerWidth / 2;
  S.toolbarContainer.style.transition = animate ? "all .2s ease" : "none";
  S.toolbarContainer.style.top = cy - 20 + "px";

  if (onRight) {
    S.toolbarContainer.style.left = "auto";
    S.toolbarContainer.style.right = window.innerWidth - cx - 20 + "px";
  } else {
    S.toolbarContainer.style.right = "auto";
    S.toolbarContainer.style.left = cx - 20 + "px";
  }

  S.toolbarContainer.classList.toggle(PREFIX + "toolbar-right", onRight);
  S.toolbarContainer.classList.toggle(PREFIX + "toolbar-left", !onRight);
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
  let cx = S.fabCX;
  let cy = S.fabCY;
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
