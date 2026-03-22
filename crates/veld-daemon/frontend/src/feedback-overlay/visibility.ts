import { S } from "./state";
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
  S.hidden = true;
  try { sessionStorage.setItem("veld-feedback-hidden", "1"); } catch (_) {}
  S.toolbarContainer.classList.add(PREFIX + "hidden");
  Object.keys(S.pins).forEach((id) => {
    S.pins[id].classList.add(PREFIX + "hidden");
  });
  S.overlay.classList.remove(PREFIX + "overlay-active");
  S.hoverOutline.style.display = "none";
  S.componentTraceEl.style.display = "none";
  if (setModeFn) setModeFn(null);
  if (S.panelOpen && togglePanelFn) togglePanelFn();
}

export function showOverlay(): void {
  S.hidden = false;
  try { sessionStorage.removeItem("veld-feedback-hidden"); } catch (_) {}
  S.toolbarContainer.classList.remove(PREFIX + "hidden");
  Object.keys(S.pins).forEach((id) => {
    S.pins[id].classList.remove(PREFIX + "hidden");
  });
}
