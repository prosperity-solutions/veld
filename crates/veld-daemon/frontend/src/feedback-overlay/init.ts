// Initialization — wires all module dependencies and starts the overlay.
import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { buildDOM } from "./dom";
import { restoreFabPos, clampFabToViewport } from "./fab";
import { onKeyDown } from "./keyboard";
import { pollEvents, pollListenStatus, loadThreads, primeEventSeq } from "./polling";
import { pollShareStatus } from "./sharing";
import { togglePanel, renderPanel, openThreadInPanel, syncPanelSideClass, applyPanelLayout } from "./panel";
import { setMode } from "./modes";
import { toggleToolbar } from "./toolbar";
import { togglePageComment, closeActivePopover, showCreatePopover } from "./popover";
import { hideOverlay, showOverlay } from "./visibility";
import { addPin, removePin, renderAllPins, scheduleReposition } from "./pins";
import { scrollToThread, checkPendingScroll, onNavigate } from "./navigation";
import { captureScreenshot, repositionFrozenFrame } from "./screenshot";
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
  syncPanelSideClass();
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
    applyPanelLayout(); // re-clamp panel width / dock margin to the new viewport
    repositionFrozenFrame(); // keep the screenshot frame in sync if mid-selection
  });
  window.addEventListener("popstate", onNavigate);

  loadThreads();
  primeEventSeq(); // baseline the cursor so a reload doesn't replay old toasts
  pollListenStatus();
  pollShareStatus(); // light up the Sharing dot if this page is already web-shared
  setInterval(pollEvents, 3000);
  setInterval(pollListenStatus, 5000);
  setInterval(pollShareStatus, 5000);

  if ("Notification" in window && Notification.permission === "default") {
    Notification.requestPermission();
  }
}
