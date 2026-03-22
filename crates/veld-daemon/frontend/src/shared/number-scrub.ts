/**
 * Bret Victor-style number scrubbing.
 *
 * Alt+drag on a number input to scrub its value.
 * Shift = 10x speed, Ctrl = 0.1x precision.
 *
 * This module provides:
 * - `computeScrubValue` — pure math function (testable)
 * - `attachScrub` — DOM attachment (imperative)
 */

export interface ScrubOptions {
  min?: number;
  max?: number;
  step?: number;
}

/**
 * Compute new value after a scrub drag.
 *
 * @param baseValue — the value when drag started
 * @param deltaPixels — horizontal mouse movement in pixels
 * @param multiplier — 1 = normal, 10 = shift, 0.1 = ctrl
 * @param opts — min/max/step constraints
 */
export function computeScrubValue(
  baseValue: number,
  deltaPixels: number,
  multiplier: number,
  opts: ScrubOptions,
): number {
  const step = opts.step || 1;
  const delta = deltaPixels * step * multiplier;
  let value = baseValue + delta;
  if (opts.min !== undefined) value = Math.max(opts.min, value);
  if (opts.max !== undefined) value = Math.min(opts.max, value);
  // Round to step precision
  value = Math.round(value / step) * step;
  return value;
}

/**
 * Attach scrub behavior to an HTML input element.
 * Returns a cleanup function.
 *
 * When the user holds Alt and drags on the input:
 * - Cursor changes to col-resize
 * - Horizontal drag adjusts the value
 * - onChange is called with the new value on every frame
 */
export function attachScrub(
  input: HTMLInputElement,
  opts: ScrubOptions,
  onChange: (value: number) => void,
): () => void {
  let scrubbing = false;
  let startX = 0;
  let startValue = 0;

  function onMouseDown(e: MouseEvent): void {
    if (!e.altKey) return;
    e.preventDefault();
    scrubbing = true;
    startX = e.clientX;
    startValue = parseFloat(input.value) || 0;
    input.style.cursor = "col-resize";
    document.body.style.cursor = "col-resize";
    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);
  }

  function onMouseMove(e: MouseEvent): void {
    if (!scrubbing) return;
    const delta = e.clientX - startX;
    const multiplier = e.shiftKey ? 10 : e.ctrlKey || e.metaKey ? 0.1 : 1;
    const newValue = computeScrubValue(startValue, delta, multiplier, opts);
    input.value = String(newValue);
    onChange(newValue);
  }

  function onMouseUp(): void {
    scrubbing = false;
    input.style.cursor = "";
    document.body.style.cursor = "";
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onMouseUp);
  }

  input.addEventListener("mousedown", onMouseDown);

  // Show col-resize cursor hint when Alt is held over the input
  function onMouseEnter(e: MouseEvent): void {
    if (e.altKey) input.style.cursor = "col-resize";
  }
  function onMouseLeave(): void {
    if (!scrubbing) input.style.cursor = "";
  }
  function onKeyDown(e: KeyboardEvent): void {
    if (e.key === "Alt") input.style.cursor = "col-resize";
  }
  function onKeyUp(e: KeyboardEvent): void {
    if (e.key === "Alt" && !scrubbing) input.style.cursor = "";
  }
  input.addEventListener("mouseenter", onMouseEnter);
  input.addEventListener("mouseleave", onMouseLeave);
  document.addEventListener("keydown", onKeyDown);
  document.addEventListener("keyup", onKeyUp);

  return () => {
    input.removeEventListener("mousedown", onMouseDown);
    input.removeEventListener("mouseenter", onMouseEnter);
    input.removeEventListener("mouseleave", onMouseLeave);
    document.removeEventListener("keydown", onKeyDown);
    document.removeEventListener("keyup", onKeyUp);
    document.removeEventListener("mousemove", onMouseMove);
    document.removeEventListener("mouseup", onMouseUp);
  };
}
