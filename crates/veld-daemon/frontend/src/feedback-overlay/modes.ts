import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { UIMode } from "./types";
import { PREFIX } from "./constants";
import { closeActivePopover } from "./popover";
import { beginScreenshotCapture, clearFrozenFrame, stopCaptureStream } from "./screenshot";
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
    clearFrozenFrame();
    stopCaptureStream();
  }
  closeActivePopover();
  dispatch({ type: "SET_MODE", mode });

  refs.toolBtnSelect.classList.toggle(PREFIX + "tool-active", mode === "select-element");
  refs.toolBtnScreenshot.classList.toggle(PREFIX + "tool-active", mode === "screenshot");

  if (mode === "select-element") {
    refs.overlay.classList.add(PREFIX + "overlay-active");
  }
  if (mode === "screenshot") {
    // Close the menu so the arc doesn't float over the capture region.
    closeToolbar();
    window.focus();
    // Freeze-first: acquire the display stream, grab one frame, stop the stream,
    // then let the user draw the selection rectangle over the frozen image. This
    // makes the selection surface and the captured pixels identical — no offset
    // from the browser's share banner shifting the layout mid-capture.
    beginScreenshotCapture();
  }
}
