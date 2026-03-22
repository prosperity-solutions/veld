/**
 * XY Pad — a 2D control surface created by fusing two numeric controls.
 *
 * This is a UI-only concept: the agent never sends "xy-pad" — the user
 * drags two compatible numeric controls together to create one, and can
 * split them back apart at any time.
 */

import type { AxisDef, VeldControls } from "../shared/controls";
import { PREFIX } from "./constants";

export interface XYPadResult {
  element: HTMLElement;
  cleanup: () => void;
}

/**
 * Create an XY pad from two axis definitions.
 * Returns the DOM element and a cleanup function.
 */
export function createXYPad(
  xAxis: AxisDef,
  yAxis: AxisDef,
  registry: VeldControls,
  onSplit: () => void,
): XYPadResult {
  const cleanups: (() => void)[] = [];

  registry.set(xAxis.name, xAxis.value);
  registry.set(yAxis.name, yAxis.value);

  const wrapper = document.createElement("div");
  wrapper.className = PREFIX + "xy-wrapper";

  // Header with axis names and split button
  const header = document.createElement("div");
  header.className = PREFIX + "xy-header";

  const title = document.createElement("span");
  title.className = PREFIX + "xy-title";
  title.textContent = (xAxis.label || xAxis.name) + " \u00d7 " + (yAxis.label || yAxis.name);
  header.appendChild(title);

  const splitBtn = document.createElement("button");
  splitBtn.className = PREFIX + "xy-split-btn";
  splitBtn.textContent = "Split";
  splitBtn.title = "Split back into two controls";
  splitBtn.addEventListener("click", onSplit);
  header.appendChild(splitBtn);

  wrapper.appendChild(header);

  // The pad itself
  const pad = document.createElement("div");
  pad.className = PREFIX + "xy-pad";

  const dot = document.createElement("div");
  dot.className = PREFIX + "xy-dot";
  pad.appendChild(dot);

  const xLabel = document.createElement("span");
  xLabel.className = PREFIX + "xy-label-x";
  pad.appendChild(xLabel);

  const yLabel = document.createElement("span");
  yLabel.className = PREFIX + "xy-label-y";
  pad.appendChild(yLabel);

  function axisToNorm(axis: AxisDef, value: number): number {
    const range = axis.max - axis.min;
    if (range === 0) return 0.5;
    return (value - axis.min) / range;
  }

  function normToAxis(axis: AxisDef, norm: number): number {
    const raw = norm * (axis.max - axis.min) + axis.min;
    const clamped = Math.max(axis.min, Math.min(axis.max, raw));
    const step = axis.step || 1;
    return Math.round(clamped / step) * step;
  }

  function updateDisplay(xVal: number, yVal: number): void {
    const nx = axisToNorm(xAxis, xVal);
    const ny = axisToNorm(yAxis, yVal);
    dot.style.left = (nx * 100) + "%";
    dot.style.top = ((1 - ny) * 100) + "%";
    xLabel.textContent = (xAxis.label || xAxis.name) + ": " + xVal + (xAxis.unit ? " " + xAxis.unit : "");
    yLabel.textContent = (yAxis.label || yAxis.name) + ": " + yVal + (yAxis.unit ? " " + yAxis.unit : "");
  }

  updateDisplay(xAxis.value, yAxis.value);

  function handlePointer(e: PointerEvent): void {
    const rect = pad.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return;
    const px = (e.clientX - rect.left) / rect.width;
    const py = (e.clientY - rect.top) / rect.height;
    const xVal = normToAxis(xAxis, Math.max(0, Math.min(1, px)));
    const yVal = normToAxis(yAxis, Math.max(0, Math.min(1, 1 - py)));
    registry.set(xAxis.name, xVal);
    registry.set(yAxis.name, yVal);
    updateDisplay(xVal, yVal);
  }

  function onDown(e: PointerEvent): void {
    if (pad.setPointerCapture) pad.setPointerCapture(e.pointerId);
    handlePointer(e);
    pad.addEventListener("pointermove", handlePointer);
  }

  function onUp(e: PointerEvent): void {
    if (pad.releasePointerCapture) pad.releasePointerCapture(e.pointerId);
    pad.removeEventListener("pointermove", handlePointer);
    handlePointer(e);
  }

  pad.addEventListener("pointerdown", onDown);
  pad.addEventListener("pointerup", onUp);

  cleanups.push(() => {
    pad.removeEventListener("pointerdown", onDown);
    pad.removeEventListener("pointerup", onUp);
    pad.removeEventListener("pointermove", handlePointer);
  });

  wrapper.appendChild(pad);

  return {
    element: wrapper,
    cleanup: () => cleanups.forEach((fn) => fn()),
  };
}
