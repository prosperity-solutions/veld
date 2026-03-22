import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { UIMode } from "./types";
import { PREFIX } from "./constants";

// Late-bound deps
let setModeFn: (mode: UIMode) => void;
let togglePanelFn: () => void;

export function setVisibilityDeps(deps: {
  setMode: (mode: UIMode) => void;
  togglePanel: () => void;
}): void {
  setModeFn = deps.setMode;
  togglePanelFn = deps.togglePanel;
}

export function hideOverlay(): void {
  dispatch({ type: "SET_HIDDEN", hidden: true });
  try { sessionStorage.setItem("veld-feedback-hidden", "1"); } catch (_) {}
  refs.toolbarContainer.classList.add(PREFIX + "hidden");
  Object.keys(getState().pins).forEach((id) => {
    getState().pins[id].classList.add(PREFIX + "hidden");
  });
  refs.overlay.classList.remove(PREFIX + "overlay-active");
  refs.hoverOutline.style.display = "none";
  refs.componentTraceEl.style.display = "none";
  if (setModeFn) setModeFn(null);
  if (getState().panelOpen && togglePanelFn) togglePanelFn();
}

export function showOverlay(): void {
  dispatch({ type: "SET_HIDDEN", hidden: false });
  try { sessionStorage.removeItem("veld-feedback-hidden"); } catch (_) {}
  refs.toolbarContainer.classList.remove(PREFIX + "hidden");
  Object.keys(getState().pins).forEach((id) => {
    getState().pins[id].classList.remove(PREFIX + "hidden");
  });
}
