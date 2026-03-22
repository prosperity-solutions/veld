import { S } from "./state";
import type { UIMode } from "./types";
import { mkEl } from "./helpers";
import { PREFIX } from "./constants";
import { attachTooltip } from "./tooltip";

// Late-bound deps — set by init.ts to avoid circular imports
let setModeFn: (mode: UIMode) => void;
let togglePageCommentFn: () => void;
let togglePanelFn: () => void;
let hideOverlayFn: () => void;

export function setToolbarDeps(deps: {
  setMode: (mode: UIMode) => void;
  togglePageComment: () => void;
  togglePanel: () => void;
  hideOverlay: () => void;
}): void {
  setModeFn = deps.setMode;
  togglePageCommentFn = deps.togglePageComment;
  togglePanelFn = deps.togglePanel;
  hideOverlayFn = deps.hideOverlay;
}

export function makeToolBtn(action: string, iconSvg: string, title: string): HTMLElement {
  const btn = mkEl("button", "tool-btn");
  (btn as HTMLElement & { dataset: DOMStringMap }).dataset.action = action;
  btn.innerHTML = iconSvg;
  attachTooltip(btn, title);
  btn.addEventListener("click", (e: Event) => {
    e.stopPropagation();
    handleToolAction(action);
  });
  return btn;
}

function handleToolAction(action: string): void {
  if (action === "select-element") {
    setModeFn(S.activeMode === "select-element" ? null : "select-element");
  } else if (action === "screenshot") {
    setModeFn(S.activeMode === "screenshot" ? null : "screenshot");
  } else if (action === "draw") {
    setModeFn(S.activeMode === "draw" ? null : "draw");
  } else if (action === "page-comment") {
    togglePageCommentFn();
  } else if (action === "show-comments") {
    togglePanelFn();
  } else if (action === "hide") {
    hideOverlayFn();
  }
}

export function toggleToolbar(): void {
  S.toolbarOpen = !S.toolbarOpen;
  S.toolbar.classList.toggle(PREFIX + "toolbar-open", S.toolbarOpen);
  if (!S.toolbarOpen && setModeFn) {
    setModeFn(null);
  }
}
