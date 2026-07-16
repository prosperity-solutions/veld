import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { deps } from "../shared/registry";
import { closeToolbar } from "./toolbar";
import { closeWebShareCard } from "./web-share";

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
  deps().setMode(null);
  closeToolbar(); // collapse the arc so it doesn't tick behind the hidden overlay
  closeWebShareCard(); // it floats outside the toolbar container and polls — must not outlive the overlay
  if (getState().panelOpen) deps().togglePanel();
}

export function showOverlay(): void {
  dispatch({ type: "SET_HIDDEN", hidden: false });
  try { sessionStorage.removeItem("veld-feedback-hidden"); } catch (_) {}
  refs.toolbarContainer.classList.remove(PREFIX + "hidden");
  Object.keys(getState().pins).forEach((id) => {
    getState().pins[id].classList.remove(PREFIX + "hidden");
  });
}
