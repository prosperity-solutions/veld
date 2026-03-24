import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { mkEl } from "./helpers";
import { PREFIX } from "./constants";
import { attachTooltip } from "./tooltip";
import { updateBadge } from "./badge";
import { deps } from "../shared/registry";

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
    deps().setMode(getState().activeMode === "select-element" ? null : "select-element");
  } else if (action === "screenshot") {
    deps().setMode(getState().activeMode === "screenshot" ? null : "screenshot");
  } else if (action === "draw") {
    deps().setMode(getState().activeMode === "draw" ? null : "draw");
  } else if (action === "page-comment") {
    deps().togglePageComment();
  } else if (action === "show-comments") {
    deps().togglePanel();
  } else if (action === "hide") {
    deps().hideOverlay();
  }
}

export function toggleToolbar(): void {
  dispatch({ type: "SET_TOOLBAR_OPEN", open: !getState().toolbarOpen });
  refs.toolbar.classList.toggle(PREFIX + "toolbar-open", getState().toolbarOpen);
  if (!getState().toolbarOpen) {
    deps().setMode(null);
    // Collapse the overflow menu when closing the toolbar.
    if (refs.toolbarOverflow) {
      refs.toolbarOverflow.classList.remove(PREFIX + "toolbar-overflow-open");
    }
  }
  updateBadge();
}
