// Initialization — wires all module dependencies and starts the overlay.
import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { buildDOM } from "./dom";
import { restoreFabPos, clampFabToViewport } from "./fab";
import { onKeyDown } from "./keyboard";
import { pollEvents, pollListenStatus, loadThreads } from "./polling";
import { togglePanel, renderPanel, openThreadInPanel } from "./panel";
import { setMode } from "./modes";
import { toggleToolbar } from "./toolbar";
import { togglePageComment, closeActivePopover, showCreatePopover } from "./popover";
import { hideOverlay, showOverlay } from "./visibility";
import { addPin, removePin, renderAllPins, scheduleReposition } from "./pins";
import { scrollToThread, checkPendingScroll, onNavigate } from "./navigation";
import { captureScreenshot } from "./screenshot";
import { ensureDrawScript } from "./draw-mode";
import { positionTooltip } from "./tooltip";
import { updateBadge } from "./badge";
import { registerDeps } from "../shared/registry";

function wireDeps(): void {
  registerDeps({
    setMode,
    toggleToolbar,
    togglePanel,
    togglePageComment,
    hideOverlay,
    showOverlay,
    closeActivePopover,
    addPin,
    removePin,
    renderAllPins,
    renderPanel,
    openThreadInPanel,
    scrollToThread,
    checkPendingScroll,
    updateBadge,
    captureScreenshot,
    showCreatePopover,
    positionTooltip,
    ensureDrawScript,
  });
}

export function init(): void {
  try {
    if (sessionStorage.getItem("veld-hidden") === "1") {
      dispatch({ type: "SET_HIDDEN", hidden: true });
    }
  } catch (_) { /* ignore */ }

  wireDeps();
  buildDOM();
  restoreFabPos();
  clampFabToViewport();

  if (getState().hidden) {
    refs.toolbarContainer.classList.add(PREFIX + "hidden");
  }

  document.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("scroll", scheduleReposition, true);
  window.addEventListener("resize", () => {
    scheduleReposition();
    clampFabToViewport();
  });
  window.addEventListener("popstate", onNavigate);

  loadThreads();
  pollEvents();
  pollListenStatus();
  setInterval(pollEvents, 3000);
  setInterval(pollListenStatus, 5000);

  if ("Notification" in window && Notification.permission === "default") {
    Notification.requestPermission();
  }
}
