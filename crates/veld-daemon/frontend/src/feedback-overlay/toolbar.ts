import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { mkEl } from "./helpers";
import { PREFIX } from "./constants";
import { attachTooltip } from "./tooltip";
import { updateBadge } from "./badge";
import { deps } from "../shared/registry";

const RADIUS = 48;           // center-to-center distance from FAB
const ARC_SPAN = Math.PI;    // 180° arc
const ARC_THICKNESS = 32;    // arc backdrop thickness
const SVG_NS = "http://www.w3.org/2000/svg";

// Arc backdrop SVG elements (created lazily)
let arcSvg: SVGSVGElement | null = null;
let arcPathEl: SVGPathElement | null = null;

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

/**
 * Get the currently active button set.
 * When overflow is open, show overflow buttons + the ⋯ toggle.
 * Otherwise, show primary buttons (filtering hidden listening dot).
 */
function getActiveButtons(): HTMLElement[] {
  if (getState().overflowOpen) {
    // Overflow page: overflow buttons + the ⋯ toggle (to switch back)
    return [...refs.overflowButtons, refs.moreBtn];
  }
  // Primary page: primary tools, conditional listening, ⋯ toggle
  return refs.radialButtons.filter(function (btn) {
    if (btn === refs.listeningModule && !getState().agentListening) return false;
    return true;
  });
}

/** Build an SVG arc (ring segment) path string with rounded endcaps. */
function arcPath(
  centerX: number, centerY: number,
  radius: number, thickness: number,
  startAngle: number, endAngle: number,
): string {
  const rOuter = radius + thickness / 2;
  const rInner = radius - thickness / 2;
  const capR = (rOuter - rInner) / 2;
  const pad = capR * 0.35 / radius;
  const a1 = startAngle - pad;
  const a2 = endAngle + pad;
  const largeArc = Math.abs(a2 - a1) > Math.PI ? 1 : 0;

  const ox1 = centerX + Math.cos(a1) * rOuter;
  const oy1 = centerY + Math.sin(a1) * rOuter;
  const ox2 = centerX + Math.cos(a2) * rOuter;
  const oy2 = centerY + Math.sin(a2) * rOuter;
  const ix2 = centerX + Math.cos(a2) * rInner;
  const iy2 = centerY + Math.sin(a2) * rInner;
  const ix1 = centerX + Math.cos(a1) * rInner;
  const iy1 = centerY + Math.sin(a1) * rInner;

  return [
    "M", ox1, oy1,
    "A", rOuter, rOuter, 0, largeArc, 1, ox2, oy2,
    "A", capR, capR, 0, 0, 1, ix2, iy2,
    "A", rInner, rInner, 0, largeArc, 0, ix1, iy1,
    "A", capR, capR, 0, 0, 1, ox1, oy1,
    "Z"
  ].join(" ");
}

/** Ensure the arc SVG exists, create lazily. */
function ensureArc(): { svg: SVGSVGElement; path: SVGPathElement } {
  if (!arcSvg) {
    arcSvg = document.createElementNS(SVG_NS, "svg");
    arcSvg.style.cssText = "position:absolute;top:0;left:0;pointer-events:none;overflow:visible;width:40px;height:40px;z-index:0;";
    arcPathEl = document.createElementNS(SVG_NS, "path");
    arcPathEl.setAttribute("fill", "var(--vf-bg)");
    arcPathEl.setAttribute("stroke", "var(--vf-border)");
    arcPathEl.setAttribute("stroke-width", "1");
    arcPathEl.style.filter = "drop-shadow(0 2px 8px rgba(0,0,0,.25))";
    arcPathEl.style.transition = "d .2s ease";
    arcSvg.appendChild(arcPathEl);
    refs.toolbarContainer.insertBefore(arcSvg, refs.toolbarContainer.firstChild);
  }
  return { svg: arcSvg, path: arcPathEl! };
}

/** Hide all buttons (both primary and overflow). */
function hideAllButtons(): void {
  const all = [...refs.radialButtons, ...refs.overflowButtons];
  all.forEach(function (btn) {
    btn.classList.remove(PREFIX + "radial-open");
    btn.style.transform = "translate(0, 0) scale(0)";
  });
}

/**
 * Position the active button set around the FAB.
 * Called on open, during drag, on overflow toggle, and on window resize.
 */
export function positionRadialButtons(): void {
  if (!getState().toolbarOpen) return;
  const baseAngle = computeBaseAngle();
  const active = getActiveButtons();
  const count = active.length;
  if (count === 0) return;

  const startAngle = baseAngle - ARC_SPAN / 2;
  const step = count > 1 ? ARC_SPAN / (count - 1) : 0;
  const cx = 20, cy = 20;

  for (let i = 0; i < count; i++) {
    const angle = startAngle + step * i;
    const x = Math.cos(angle) * RADIUS;
    const y = Math.sin(angle) * RADIUS;
    active[i].style.transform = "translate(" + Math.round(x) + "px, " + Math.round(y) + "px) scale(1)";
  }

  // Update arc backdrop
  const { path } = ensureArc();
  const endAngle = startAngle + step * (count - 1);
  path.setAttribute("d", arcPath(cx, cy, RADIUS, ARC_THICKNESS, startAngle, endAngle));
}

/** Toggle between primary and overflow button sets in the same arc. */
export function toggleOverflow(): void {
  const nowOpen = !getState().overflowOpen;
  dispatch({ type: "SET_OVERFLOW_OPEN", open: nowOpen });

  // Hide the old set, show the new set
  hideAllButtons();
  positionRadialButtons();
  const active = getActiveButtons();
  active.forEach(function (btn, i) {
    setTimeout(function () {
      btn.classList.add(PREFIX + "radial-open");
    }, i * 30);
  });
}

export function toggleToolbar(): void {
  dispatch({ type: "SET_TOOLBAR_OPEN", open: !getState().toolbarOpen });

  if (getState().toolbarOpen) {
    positionRadialButtons();
    if (arcSvg) arcSvg.style.opacity = "1";
    const active = getActiveButtons();
    active.forEach(function (btn, i) {
      setTimeout(function () {
        btn.classList.add(PREFIX + "radial-open");
      }, i * 30);
    });
  } else {
    deps().setMode(null);
    dispatch({ type: "SET_OVERFLOW_OPEN", open: false });
    const active = getActiveButtons();
    const total = active.length;
    active.forEach(function (btn, i) {
      setTimeout(function () {
        btn.classList.remove(PREFIX + "radial-open");
        btn.style.transform = "translate(0, 0) scale(0)";
      }, (total - 1 - i) * 30);
    });
    // Also hide any buttons from the other set
    hideAllButtons();
    setTimeout(function () {
      if (arcSvg) arcSvg.style.opacity = "0";
    }, total * 30);
  }

  updateBadge();
}
