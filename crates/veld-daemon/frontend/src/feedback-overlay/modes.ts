import { refs } from "./refs";
import { store, dispatch } from "./store";
import type { UIMode } from "./types";
import { PREFIX } from "./constants";
import { toast } from "./toast";
import { closeActivePopover } from "./popover";
import { stopCaptureStream, acquireCaptureStream } from "./screenshot";
import { ensureDrawScript, setupGlobalDrawCanvas, teardownGlobalDrawCanvas } from "./draw-mode";
import { toggleToolbar } from "./toolbar";

export function setMode(mode: UIMode): void {
  // Tear down previous mode
  if (store.activeMode === "select-element") {
    refs.overlay.classList.remove(PREFIX + "overlay-active");
    refs.hoverOutline.style.display = "none";
    refs.componentTraceEl.style.display = "none";
    dispatch({ type: "SET_HOVERED", el: null });
    dispatch({ type: "SET_LOCKED", el: null });
  }
  if (store.activeMode === "screenshot") {
    refs.overlay.classList.remove(PREFIX + "overlay-active");
    refs.overlay.classList.remove(PREFIX + "overlay-crosshair");
    refs.screenshotRect.style.display = "none";
    stopCaptureStream();
  }
  if (store.activeMode === "draw") {
    teardownGlobalDrawCanvas();
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
    acquireCaptureStream().then(() => {
      refs.overlay.classList.add(PREFIX + "overlay-active");
      refs.overlay.classList.add(PREFIX + "overlay-crosshair");
      window.focus();
      toast("Draw a rectangle to capture a screenshot");
    }).catch(() => {
      toast("Screen capture denied", true);
      dispatch({ type: "SET_MODE", mode: null });
      refs.toolBtnScreenshot.classList.remove(PREFIX + "tool-active");
    });
  }
  if (mode === "draw") {
    if (store.toolbarOpen) toggleToolbar();
    acquireCaptureStream().then(() => ensureDrawScript()).then(() => {
      setupGlobalDrawCanvas();
      window.focus();
    }).catch(() => {
      toast("Screen capture denied", true);
      dispatch({ type: "SET_MODE", mode: null });
      refs.toolBtnDraw.classList.remove(PREFIX + "tool-active");
    });
  }
}
