import { getState } from "./store";
import { modKey } from "./helpers";
import { deps } from "../shared/registry";

export function onKeyDown(e: KeyboardEvent): void {
  // ESC in draw mode is handled by the draw overlay itself (shows confirm bar)
  if (e.key === "Escape" && getState().activeMode === "draw") {
    return; // let draw overlay's keydown handler deal with it
  }

  if (getState().shortcutsDisabled) return;

  const mod = modKey(e) && e.shiftKey;

  // Mod+Shift+V: toggle toolbar (or bring back from hidden)
  if (mod && e.code === "KeyV") {
    e.preventDefault();
    if (getState().hidden) { deps().showOverlay(); return; }
    deps().toggleToolbar();
    return;
  }

  // Mod+Shift+.: toggle overlay visibility
  if (mod && e.code === "Period") {
    e.preventDefault();
    if (getState().hidden) { deps().showOverlay(); } else { deps().hideOverlay(); }
    return;
  }

  if (getState().hidden) return;

  // Mod+Shift+F: select element mode
  if (mod && e.code === "KeyF") {
    e.preventDefault();
    if (!getState().toolbarOpen) deps().toggleToolbar();
    deps().setMode(getState().activeMode === "select-element" ? null : "select-element");
    return;
  }

  // Mod+Shift+S: screenshot mode
  if (mod && e.code === "KeyS") {
    e.preventDefault();
    if (!getState().toolbarOpen) deps().toggleToolbar();
    deps().setMode(getState().activeMode === "screenshot" ? null : "screenshot");
    return;
  }

  // Mod+Shift+D: draw mode
  if (mod && e.code === "KeyD") {
    e.preventDefault();
    if (!getState().toolbarOpen) deps().toggleToolbar();
    deps().setMode(getState().activeMode === "draw" ? null : "draw");
    return;
  }

  // Mod+Shift+P: page comment
  if (mod && e.code === "KeyP") {
    e.preventDefault();
    if (!getState().toolbarOpen) deps().toggleToolbar();
    deps().togglePageComment();
    return;
  }

  // Mod+Shift+C: toggle panel
  if (mod && e.code === "KeyC") {
    e.preventDefault();
    deps().togglePanel();
    return;
  }

  // Escape: cascading dismiss
  if (e.key === "Escape") {
    if (getState().activePopover) {
      deps().closeActivePopover();
    } else if (getState().activeMode) {
      deps().setMode(null);
    } else if (getState().toolbarOpen) {
      deps().toggleToolbar();
    }
  }
}
