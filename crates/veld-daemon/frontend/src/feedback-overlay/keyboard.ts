import { S } from "./state";
import { modKey } from "./helpers";
import type { UIMode } from "./types";

// Late-bound deps to avoid circular imports
export let setModeFn: (mode: UIMode) => void;
export let toggleToolbarFn: () => void;
export let togglePageCommentFn: () => void;
export let togglePanelFn: () => void;
export let hideOverlayFn: () => void;
export let showOverlayFn: () => void;
export let closeActivePopoverFn: () => void;

export function setKeyboardDeps(deps: {
  setMode: typeof setModeFn;
  toggleToolbar: typeof toggleToolbarFn;
  togglePageComment: typeof togglePageCommentFn;
  togglePanel: typeof togglePanelFn;
  hideOverlay: typeof hideOverlayFn;
  showOverlay: typeof showOverlayFn;
  closeActivePopover: typeof closeActivePopoverFn;
}) {
  setModeFn = deps.setMode;
  toggleToolbarFn = deps.toggleToolbar;
  togglePageCommentFn = deps.togglePageComment;
  togglePanelFn = deps.togglePanel;
  hideOverlayFn = deps.hideOverlay;
  showOverlayFn = deps.showOverlay;
  closeActivePopoverFn = deps.closeActivePopover;
}

export function onKeyDown(e: KeyboardEvent): void {
  // ESC exits draw mode (always, even with shortcuts disabled)
  if (e.key === "Escape" && S.activeMode === "draw") {
    e.preventDefault();
    setModeFn(null);
    return;
  }

  if (S.shortcutsDisabled) return;

  const mod = modKey(e) && e.shiftKey;

  // Mod+Shift+V: toggle toolbar (or bring back from hidden)
  if (mod && e.code === "KeyV") {
    e.preventDefault();
    if (S.hidden) { showOverlayFn(); return; }
    toggleToolbarFn();
    return;
  }

  // Mod+Shift+.: toggle overlay visibility
  if (mod && e.code === "Period") {
    e.preventDefault();
    if (S.hidden) { showOverlayFn(); } else { hideOverlayFn(); }
    return;
  }

  if (S.hidden) return;

  // Mod+Shift+F: select element mode
  if (mod && e.code === "KeyF") {
    e.preventDefault();
    if (!S.toolbarOpen) toggleToolbarFn();
    setModeFn(S.activeMode === "select-element" ? null : "select-element");
    return;
  }

  // Mod+Shift+S: screenshot mode
  if (mod && e.code === "KeyS") {
    e.preventDefault();
    if (!S.toolbarOpen) toggleToolbarFn();
    setModeFn(S.activeMode === "screenshot" ? null : "screenshot");
    return;
  }

  // Mod+Shift+D: draw mode
  if (mod && e.code === "KeyD") {
    e.preventDefault();
    if (!S.toolbarOpen) toggleToolbarFn();
    setModeFn(S.activeMode === "draw" ? null : "draw");
    return;
  }

  // Mod+Shift+P: page comment
  if (mod && e.code === "KeyP") {
    e.preventDefault();
    if (!S.toolbarOpen) toggleToolbarFn();
    togglePageCommentFn();
    return;
  }

  // Mod+Shift+C: toggle panel
  if (mod && e.code === "KeyC") {
    e.preventDefault();
    togglePanelFn();
    return;
  }

  // Escape: cascading dismiss
  if (e.key === "Escape") {
    if (S.activePopover) {
      closeActivePopoverFn();
    } else if (S.activeMode) {
      setModeFn(null);
    } else if (S.toolbarOpen) {
      toggleToolbarFn();
    }
  }
}
