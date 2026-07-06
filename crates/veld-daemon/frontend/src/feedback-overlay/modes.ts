import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { UIMode } from "./types";
import { PREFIX } from "./constants";
import { toast } from "./toast";
import { closeActivePopover } from "./popover";
import { stopCaptureStream } from "./screenshot";
import { ensureDrawScript, setupGlobalDrawCanvas, teardownGlobalDrawCanvas } from "./draw-mode";
import { closeToolbar } from "./toolbar";

export function setMode(mode: UIMode): void {
  // Tear down previous mode
  if (getState().activeMode === "select-element") {
    refs.overlay.classList.remove(PREFIX + "overlay-active");
    refs.hoverOutline.style.display = "none";
    refs.componentTraceEl.style.display = "none";
    dispatch({ type: "SET_HOVERED", el: null });
    dispatch({ type: "SET_LOCKED", el: null });
  }
  if (getState().activeMode === "screenshot") {
    refs.overlay.classList.remove(PREFIX + "overlay-active");
    refs.overlay.classList.remove(PREFIX + "overlay-crosshair");
    refs.screenshotRect.style.display = "none";
    stopCaptureStream();
  }
  if (getState().activeMode === "draw") {
    // Inline cleanup — teardownGlobalDrawCanvas() fails silently for unknown
    // reasons, but the same cleanup done inline (as in draw-mode.ts onDone)
    // works reliably. Match that proven pattern here.
    const drawCleanup = getState().drawCleanup;
    dispatch({ type: "SET_DRAW_CLEANUP", cleanup: null });
    if (drawCleanup) {
      try { drawCleanup(); } catch (e) { console.error("[veld] draw cleanup:", e); }
    }
    const dc = getState().drawCanvas;
    if (dc && dc.parentNode) dc.parentNode.removeChild(dc);
    dispatch({ type: "SET_DRAW_CANVAS", canvas: null });
    const prevOverflow = getState().prevOverflow;
    if (prevOverflow !== null) {
      document.body.style.overflow = prevOverflow;
      dispatch({ type: "SET_PREV_OVERFLOW", overflow: null });
    }
    stopCaptureStream();
  }

  closeActivePopover();
  dispatch({ type: "SET_MODE", mode });

  refs.toolBtnSelect.classList.toggle(PREFIX + "tool-active", mode === "select-element");
  refs.toolBtnScreenshot.classList.toggle(PREFIX + "tool-active", mode === "screenshot");
  refs.toolBtnDraw.classList.toggle(PREFIX + "tool-active", mode === "draw");

  if (mode === "select-element") {
    refs.overlay.classList.add(PREFIX + "overlay-active");
  }
  if (mode === "screenshot") {
    // No acquireCaptureStream — selection starts instantly without screen share dialog.
    // Capture is deferred to after the user finishes drawing the selection rectangle.
    // Close the menu so the arc doesn't float over the capture region.
    closeToolbar();
    refs.overlay.classList.add(PREFIX + "overlay-active");
    refs.overlay.classList.add(PREFIX + "overlay-crosshair");
    window.focus();
    toast("Draw a rectangle to capture a screenshot");
  }
  if (mode === "draw") {
    // Close the arc menu (the engine is the sole authority for open state).
    // Safe to call here: SET_MODE was already dispatched above, so the engine's
    // onOpenChange guard (only clears select-element) won't clobber draw mode.
    closeToolbar();
    // No acquireCaptureStream — draw starts instantly without screen share dialog.
    // Capture is deferred to Done (for compositing) or blur tool (for pixelation).
    ensureDrawScript().then(() => {
      setupGlobalDrawCanvas();
      window.focus();
    }).catch(() => {
      toast("Failed to load draw module", true);
      dispatch({ type: "SET_MODE", mode: null });
      refs.toolBtnDraw.classList.remove(PREFIX + "tool-active");
    });
  }
}
