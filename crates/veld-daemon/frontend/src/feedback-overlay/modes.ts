import { S } from "./state";
import type { UIMode } from "./types";
import { PREFIX } from "./constants";
import { toast } from "./toast";
import { closeActivePopover } from "./popover";
import { stopCaptureStream, acquireCaptureStream } from "./screenshot";
import { ensureDrawScript, setupGlobalDrawCanvas, teardownGlobalDrawCanvas } from "./draw-mode";
import { toggleToolbar } from "./toolbar";

export function setMode(mode: UIMode): void {
  // Tear down previous mode
  if (S.activeMode === "select-element") {
    S.overlay.classList.remove(PREFIX + "overlay-active");
    S.hoverOutline.style.display = "none";
    S.componentTraceEl.style.display = "none";
    S.hoveredEl = null;
    S.lockedEl = null;
  }
  if (S.activeMode === "screenshot") {
    S.overlay.classList.remove(PREFIX + "overlay-active");
    S.overlay.classList.remove(PREFIX + "overlay-crosshair");
    S.screenshotRect.style.display = "none";
    stopCaptureStream();
  }
  if (S.activeMode === "draw") {
    teardownGlobalDrawCanvas();
    stopCaptureStream();
  }

  closeActivePopover();
  S.activeMode = mode;

  S.toolBtnSelect.classList.toggle(PREFIX + "tool-active", mode === "select-element");
  S.toolBtnScreenshot.classList.toggle(PREFIX + "tool-active", mode === "screenshot");
  S.toolBtnDraw.classList.toggle(PREFIX + "tool-active", mode === "draw");

  if (mode === "select-element") {
    S.overlay.classList.add(PREFIX + "overlay-active");
  }
  if (mode === "screenshot") {
    acquireCaptureStream().then(() => {
      S.overlay.classList.add(PREFIX + "overlay-active");
      S.overlay.classList.add(PREFIX + "overlay-crosshair");
      window.focus();
      toast("Draw a rectangle to capture a screenshot");
    }).catch(() => {
      toast("Screen capture denied", true);
      S.activeMode = null;
      S.toolBtnScreenshot.classList.remove(PREFIX + "tool-active");
    });
  }
  if (mode === "draw") {
    if (S.toolbarOpen) toggleToolbar();
    acquireCaptureStream().then(() => ensureDrawScript()).then(() => {
      setupGlobalDrawCanvas();
      window.focus();
    }).catch(() => {
      toast("Screen capture denied", true);
      S.activeMode = null;
      S.toolBtnDraw.classList.remove(PREFIX + "tool-active");
    });
  }
}
