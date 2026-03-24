import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { mkEl } from "./helpers";
import { PREFIX } from "./constants";
import { attachTooltip } from "./tooltip";
import { updateBadge } from "./badge";
import { deps } from "../shared/registry";

const RADIUS = 55;          // primary ring: center-to-center distance
const OVERFLOW_RADIUS = 105; // secondary ring
const ARC_SPAN = Math.PI;   // 180° arc for primary buttons
const OVERFLOW_ARC = Math.PI / 2; // 90° arc for overflow buttons

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

/** Compute the base angle for the arc: points from FAB toward viewport center. */
function computeBaseAngle(): number {
  const cx = getState().fabCX / window.innerWidth;
  const cy = getState().fabCY / window.innerHeight;
  return Math.atan2(0.5 - cy, 0.5 - cx);
}

/** Get visible primary buttons (filters out hidden listening dot). */
function getVisiblePrimary(): HTMLElement[] {
  return refs.radialButtons.filter(function (btn) {
    // The listening module is hidden when agent is not listening
    if (btn === refs.listeningModule && !getState().agentListening) return false;
    return true;
  });
}

/**
 * Position radial buttons around the FAB.
 * Called on open, during drag, and on window resize.
 */
export function positionRadialButtons(): void {
  if (!getState().toolbarOpen) return;
  const baseAngle = computeBaseAngle();
  const visible = getVisiblePrimary();
  const count = visible.length;
  if (count === 0) return;

  const startAngle = baseAngle - ARC_SPAN / 2;
  const step = count > 1 ? ARC_SPAN / (count - 1) : 0;

  for (let i = 0; i < count; i++) {
    const angle = startAngle + step * i;
    const x = Math.cos(angle) * RADIUS;
    const y = Math.sin(angle) * RADIUS;
    visible[i].style.transform = "translate(" + Math.round(x) + "px, " + Math.round(y) + "px) scale(1)";
  }

  // Hide buttons that are not visible (e.g. listening when agent not listening)
  refs.radialButtons.forEach(function (btn) {
    if (visible.indexOf(btn) === -1) {
      btn.classList.remove(PREFIX + "radial-open");
      btn.style.transform = "translate(0, 0) scale(0)";
    }
  });

  // Position overflow buttons if open
  if (getState().overflowOpen) {
    positionOverflowButtons(baseAngle, visible);
  }
}

/** Position the second-ring overflow buttons around the ⋯ button's angle. */
function positionOverflowButtons(baseAngle: number, visiblePrimary: HTMLElement[]): void {
  // Find the angle of the ⋯ button
  const moreIndex = visiblePrimary.indexOf(refs.moreBtn);
  if (moreIndex === -1) return;
  const count = visiblePrimary.length;
  const startAngle = baseAngle - ARC_SPAN / 2;
  const step = count > 1 ? ARC_SPAN / (count - 1) : 0;
  const moreAngle = startAngle + step * moreIndex;

  const overflowCount = refs.overflowButtons.length;
  const overflowStart = moreAngle - OVERFLOW_ARC / 2;
  const overflowStep = overflowCount > 1 ? OVERFLOW_ARC / (overflowCount - 1) : 0;

  for (let i = 0; i < overflowCount; i++) {
    const angle = overflowStart + overflowStep * i;
    const x = Math.cos(angle) * OVERFLOW_RADIUS;
    const y = Math.sin(angle) * OVERFLOW_RADIUS;
    refs.overflowButtons[i].style.transform = "translate(" + Math.round(x) + "px, " + Math.round(y) + "px) scale(1)";
    refs.overflowButtons[i].classList.add(PREFIX + "radial-open");
  }
}

/** Collapse overflow buttons back to center. */
function collapseOverflowButtons(): void {
  refs.overflowButtons.forEach(function (btn) {
    btn.classList.remove(PREFIX + "radial-open");
    btn.style.transform = "translate(0, 0) scale(0)";
  });
}

export function toggleOverflow(): void {
  const nowOpen = !getState().overflowOpen;
  dispatch({ type: "SET_OVERFLOW_OPEN", open: nowOpen });
  if (nowOpen) {
    positionRadialButtons(); // will also position overflow
  } else {
    collapseOverflowButtons();
  }
}

export function toggleToolbar(): void {
  dispatch({ type: "SET_TOOLBAR_OPEN", open: !getState().toolbarOpen });

  if (getState().toolbarOpen) {
    // Open: position and stagger-reveal buttons
    positionRadialButtons();
    const visible = getVisiblePrimary();
    visible.forEach(function (btn, i) {
      setTimeout(function () {
        btn.classList.add(PREFIX + "radial-open");
      }, i * 30);
    });
  } else {
    // Close: collapse all buttons
    deps().setMode(null);
    dispatch({ type: "SET_OVERFLOW_OPEN", open: false });
    const visible = getVisiblePrimary();
    const total = visible.length;
    visible.forEach(function (btn, i) {
      setTimeout(function () {
        btn.classList.remove(PREFIX + "radial-open");
        btn.style.transform = "translate(0, 0) scale(0)";
      }, (total - 1 - i) * 30);
    });
    collapseOverflowButtons();
  }

  updateBadge();
}
