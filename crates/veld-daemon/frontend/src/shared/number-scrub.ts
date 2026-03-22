/**
 * Bret Victor-style number scrubbing.
 *
 * Alt/Option+drag on a number input to scrub its value.
 * Shift = 10x speed, Ctrl/Cmd = 0.1x precision.
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
 * When the user holds Alt/Option and drags on the input:
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

  function onPointerDown(e: PointerEvent): void {
    if (!e.altKey) return;
    e.preventDefault();
    e.stopPropagation();
    scrubbing = true;
    startX = e.clientX;
    startValue = parseFloat(input.value) || 0;
    input.style.cursor = "col-resize";
    document.body.style.cursor = "col-resize";
    input.setPointerCapture(e.pointerId);
  }

  function onPointerMove(e: PointerEvent): void {
    if (!scrubbing) return;
    e.preventDefault();
    const delta = e.clientX - startX;
    const multiplier = e.shiftKey ? 10 : e.ctrlKey || e.metaKey ? 0.1 : 1;
    const newValue = computeScrubValue(startValue, delta, multiplier, opts);
    input.value = String(newValue);
    onChange(newValue);
  }

  function onPointerUp(e: PointerEvent): void {
    if (!scrubbing) return;
    scrubbing = false;
    input.style.cursor = "";
    document.body.style.cursor = "";
    input.releasePointerCapture(e.pointerId);
  }

  // Use pointer events on the input itself — works inside Shadow DOM
  input.addEventListener("pointerdown", onPointerDown);
  input.addEventListener("pointermove", onPointerMove);
  input.addEventListener("pointerup", onPointerUp);

  // Show col-resize cursor hint when Alt/Option is held over the input
  function onKeyChange(e: KeyboardEvent): void {
    if (e.key === "Alt" && !scrubbing) {
      // Check if pointer is over this input
      input.style.cursor = e.type === "keydown" ? "col-resize" : "";
    }
  }
  input.addEventListener("keydown", onKeyChange);
  input.addEventListener("keyup", onKeyChange);

  return () => {
    input.removeEventListener("pointerdown", onPointerDown);
    input.removeEventListener("pointermove", onPointerMove);
    input.removeEventListener("pointerup", onPointerUp);
    input.removeEventListener("keydown", onKeyChange);
    input.removeEventListener("keyup", onKeyChange);
  };
}
