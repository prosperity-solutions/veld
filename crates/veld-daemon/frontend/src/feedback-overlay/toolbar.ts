import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { mkEl } from "./helpers";
import { PREFIX } from "./constants";
import { attachTooltip } from "./tooltip";
import { updateBadge } from "./badge";
import { deps } from "../shared/registry";

const RADIUS = 48;           // primary ring: center-to-center distance
const OVERFLOW_RADIUS = 85;  // secondary ring
const ARC_SPAN = Math.PI;    // 180° arc for primary buttons
const OVERFLOW_ARC = Math.PI / 2; // 90° arc for overflow buttons
const ARC_THICKNESS = 36;    // thickness of the arc backdrop (slightly > button 30px)
const SVG_NS = "http://www.w3.org/2000/svg";

// Arc backdrop SVG elements (created lazily)
let primaryArcSvg: SVGSVGElement | null = null;
let primaryArcPath: SVGPathElement | null = null;
let overflowArcSvg: SVGSVGElement | null = null;
let overflowArcPath: SVGPathElement | null = null;

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
    if (btn === refs.listeningModule && !getState().agentListening) return false;
    return true;
  });
}

/** Build an SVG arc (ring segment) path string. */
function arcPath(
  centerX: number, centerY: number,
  radius: number, thickness: number,
  startAngle: number, endAngle: number,
): string {
  const rOuter = radius + thickness / 2;
  const rInner = radius - thickness / 2;
  const capR = (rOuter - rInner) / 2;
  // Extend just enough for the rounded endcap to clear the button center
  const pad = capR / radius;
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
    // Rounded endcap at end (outer → inner)
    "A", capR, capR, 0, 0, 1, ix2, iy2,
    "A", rInner, rInner, 0, largeArc, 0, ix1, iy1,
    // Rounded endcap at start (inner → outer)
    "A", capR, capR, 0, 0, 1, ox1, oy1,
    "Z"
  ].join(" ");
}

/** Ensure the primary arc SVG exists, create lazily. */
function ensurePrimaryArc(): { svg: SVGSVGElement; path: SVGPathElement } {
  if (!primaryArcSvg) {
    primaryArcSvg = document.createElementNS(SVG_NS, "svg");
    primaryArcSvg.style.cssText = "position:absolute;top:0;left:0;pointer-events:none;overflow:visible;width:40px;height:40px;z-index:0;";
    primaryArcPath = document.createElementNS(SVG_NS, "path");
    primaryArcPath.setAttribute("fill", "var(--vf-bg)");
    primaryArcPath.setAttribute("stroke", "var(--vf-border)");
    primaryArcPath.setAttribute("stroke-width", "1");
    primaryArcPath.style.filter = "drop-shadow(0 2px 8px rgba(0,0,0,.25))";
    primaryArcPath.style.transition = "d .2s ease";
    primaryArcSvg.appendChild(primaryArcPath);
    refs.toolbarContainer.insertBefore(primaryArcSvg, refs.toolbarContainer.firstChild);
  }
  return { svg: primaryArcSvg, path: primaryArcPath! };
}

function ensureOverflowArc(): { svg: SVGSVGElement; path: SVGPathElement } {
  if (!overflowArcSvg) {
    overflowArcSvg = document.createElementNS(SVG_NS, "svg");
    overflowArcSvg.style.cssText = "position:absolute;top:0;left:0;pointer-events:none;overflow:visible;width:40px;height:40px;z-index:0;opacity:0;transition:opacity .15s;";
    overflowArcPath = document.createElementNS(SVG_NS, "path");
    overflowArcPath.setAttribute("fill", "var(--vf-bg)");
    overflowArcPath.setAttribute("stroke", "var(--vf-border)");
    overflowArcPath.setAttribute("stroke-width", "1");
    overflowArcPath.style.filter = "drop-shadow(0 2px 8px rgba(0,0,0,.25))";
    overflowArcSvg.appendChild(overflowArcPath);
    refs.toolbarContainer.insertBefore(overflowArcSvg, refs.toolbarContainer.firstChild);
  }
  return { svg: overflowArcSvg, path: overflowArcPath! };
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
  // Center of the FAB within the container (20, 20)
  const cx = 20, cy = 20;

  for (let i = 0; i < count; i++) {
    const angle = startAngle + step * i;
    const x = Math.cos(angle) * RADIUS;
    const y = Math.sin(angle) * RADIUS;
    visible[i].style.transform = "translate(" + Math.round(x) + "px, " + Math.round(y) + "px) scale(1)";
  }

  // Hide buttons that are not visible
  refs.radialButtons.forEach(function (btn) {
    if (visible.indexOf(btn) === -1) {
      btn.classList.remove(PREFIX + "radial-open");
      btn.style.transform = "translate(0, 0) scale(0)";
    }
  });

  // Update primary arc backdrop
  const { path } = ensurePrimaryArc();
  const endAngle = startAngle + step * (count - 1);
  path.setAttribute("d", arcPath(cx, cy, RADIUS, ARC_THICKNESS, startAngle, endAngle));

  // Position overflow buttons if open
  if (getState().overflowOpen) {
    positionOverflowButtons(baseAngle, visible);
  }
}

/** Position the second-ring overflow buttons, aligned to start from the ⋯ button's angle. */
function positionOverflowButtons(baseAngle: number, visiblePrimary: HTMLElement[]): void {
  const moreIndex = visiblePrimary.indexOf(refs.moreBtn);
  if (moreIndex === -1) return;
  const count = visiblePrimary.length;
  const primaryStart = baseAngle - ARC_SPAN / 2;
  const primaryStep = count > 1 ? ARC_SPAN / (count - 1) : 0;
  const moreAngle = primaryStart + primaryStep * moreIndex;
  const cx = 20, cy = 20;

  // Extend overflow in the same angular direction as the primary arc
  // (toward the arc center), so it stays within the same screen region.
  const arcMidAngle = baseAngle;
  const moreOnPositiveSide = moreAngle >= arcMidAngle;

  const overflowCount = refs.overflowButtons.length;
  const overflowStep = overflowCount > 1 ? OVERFLOW_ARC / (overflowCount - 1) : 0;
  // Start from moreAngle and extend inward (toward the arc center)
  const overflowStart = moreOnPositiveSide ? moreAngle - OVERFLOW_ARC : moreAngle;

  for (let i = 0; i < overflowCount; i++) {
    const angle = overflowStart + overflowStep * i;
    const x = Math.cos(angle) * OVERFLOW_RADIUS;
    const y = Math.sin(angle) * OVERFLOW_RADIUS;
    refs.overflowButtons[i].style.transform = "translate(" + Math.round(x) + "px, " + Math.round(y) + "px) scale(1)";
    refs.overflowButtons[i].classList.add(PREFIX + "radial-open");
  }

  // Update overflow arc backdrop
  const { svg, path } = ensureOverflowArc();
  const overflowEnd = overflowStart + overflowStep * (overflowCount - 1);
  path.setAttribute("d", arcPath(cx, cy, OVERFLOW_RADIUS, ARC_THICKNESS, overflowStart, overflowEnd));
  svg.style.opacity = "1";
}

/** Collapse overflow buttons back to center. */
function collapseOverflowButtons(): void {
  refs.overflowButtons.forEach(function (btn) {
    btn.classList.remove(PREFIX + "radial-open");
    btn.style.transform = "translate(0, 0) scale(0)";
  });
  if (overflowArcSvg) overflowArcSvg.style.opacity = "0";
}

/** Show/hide the primary arc backdrop. */
function showPrimaryArc(): void {
  const { svg } = ensurePrimaryArc();
  svg.style.opacity = "1";
}
function hidePrimaryArc(): void {
  if (primaryArcSvg) primaryArcSvg.style.opacity = "0";
}

export function toggleOverflow(): void {
  const nowOpen = !getState().overflowOpen;
  dispatch({ type: "SET_OVERFLOW_OPEN", open: nowOpen });
  if (nowOpen) {
    positionRadialButtons();
  } else {
    collapseOverflowButtons();
  }
}

export function toggleToolbar(): void {
  dispatch({ type: "SET_TOOLBAR_OPEN", open: !getState().toolbarOpen });

  if (getState().toolbarOpen) {
    positionRadialButtons();
    showPrimaryArc();
    const visible = getVisiblePrimary();
    visible.forEach(function (btn, i) {
      setTimeout(function () {
        btn.classList.add(PREFIX + "radial-open");
      }, i * 30);
    });
  } else {
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
    setTimeout(hidePrimaryArc, total * 30);
  }

  updateBadge();
}
