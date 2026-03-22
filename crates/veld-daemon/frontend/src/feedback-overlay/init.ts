// Initialization — wires all module dependencies and starts the overlay.
import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { buildDOM } from "./dom";
import { restoreFabPos, clampFabToViewport } from "./fab";
import { onKeyDown, setKeyboardDeps } from "./keyboard";
import { setBackdropDeps } from "./backdrop";
import { setPollingDeps, pollEvents, pollListenStatus, loadThreads } from "./polling";
import { setPanelDeps, togglePanel, renderPanel, openThreadInPanel } from "./panel";
import { setMode } from "./modes";
import { setToolbarDeps, toggleToolbar } from "./toolbar";
import { setPopoverDeps, togglePageComment, closeActivePopover, showCreatePopover } from "./popover";
import { setVisibilityDeps, hideOverlay, showOverlay } from "./visibility";
import { addPin, removePin, renderAllPins, scheduleReposition } from "./pins";
import { setNavigationDeps, scrollToThread, checkPendingScroll, onNavigate } from "./navigation";
import { captureScreenshot, setScreenshotDeps } from "./screenshot";
import { setDrawModeDeps, ensureDrawScript } from "./draw-mode";
import { positionTooltip } from "./tooltip";
import { updateBadge } from "./badge";

function wireDeps(): void {
  setToolbarDeps({
    setMode,
    togglePageComment,
    togglePanel,
    hideOverlay,
  });

  setVisibilityDeps({
    setMode,
    togglePanel,
  });

  setKeyboardDeps({
    setMode,
    toggleToolbar,
    togglePageComment,
    togglePanel,
    hideOverlay,
    showOverlay,
    closeActivePopover,
  });

  setBackdropDeps({
    captureScreenshot,
    showCreatePopover,
    positionTooltip,
  });

  setPopoverDeps({
    addPin,
    updateBadge,
    renderPanel,
  });

  setPollingDeps({
    addPin,
    removePin,
    renderAllPins,
    renderPanel,
    openThreadInPanel,
    scrollToThread,
    checkPendingScroll,
  });

  setPanelDeps({
    closeActivePopover,
    renderAllPins,
    addPin,
    scrollToThread,
  });

  setDrawModeDeps({
    setMode,
  });

  setScreenshotDeps({
    setMode,
    ensureDrawScript,
  });

  setNavigationDeps({
    renderAllPins,
    renderPanel,
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
