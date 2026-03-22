import { store } from "./store";
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
  if (e.key === "Escape" && store.activeMode === "draw") {
    e.preventDefault();
    setModeFn(null);
    return;
  }

  if (store.shortcutsDisabled) return;

  const mod = modKey(e) && e.shiftKey;

  // Mod+Shift+V: toggle toolbar (or bring back from hidden)
  if (mod && e.code === "KeyV") {
    e.preventDefault();
    if (store.hidden) { showOverlayFn(); return; }
    toggleToolbarFn();
    return;
  }

  // Mod+Shift+.: toggle overlay visibility
  if (mod && e.code === "Period") {
    e.preventDefault();
    if (store.hidden) { showOverlayFn(); } else { hideOverlayFn(); }
    return;
  }

  if (store.hidden) return;

  // Mod+Shift+F: select element mode
  if (mod && e.code === "KeyF") {
    e.preventDefault();
    if (!store.toolbarOpen) toggleToolbarFn();
    setModeFn(store.activeMode === "select-element" ? null : "select-element");
    return;
  }

  // Mod+Shift+S: screenshot mode
  if (mod && e.code === "KeyS") {
    e.preventDefault();
    if (!store.toolbarOpen) toggleToolbarFn();
    setModeFn(store.activeMode === "screenshot" ? null : "screenshot");
    return;
  }

  // Mod+Shift+D: draw mode
  if (mod && e.code === "KeyD") {
    e.preventDefault();
    if (!store.toolbarOpen) toggleToolbarFn();
    setModeFn(store.activeMode === "draw" ? null : "draw");
    return;
  }

  // Mod+Shift+P: page comment
  if (mod && e.code === "KeyP") {
    e.preventDefault();
    if (!store.toolbarOpen) toggleToolbarFn();
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
    if (store.activePopover) {
      closeActivePopoverFn();
    } else if (store.activeMode) {
      setModeFn(null);
    } else if (store.toolbarOpen) {
      toggleToolbarFn();
    }
  }
}
