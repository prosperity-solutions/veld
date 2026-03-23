/**
 * Renders interactive controls from agent messages into the thread panel.
 *
 * When an agent sends a message with a `controls` field, this module
 * renders sliders, number inputs, dropdowns, color pickers, toggles,
 * and buttons — all wired to window.__veld_controls.
 *
 * Numeric controls (number/slider) can be fused into an XY pad by the
 * user — drag one numeric control's grip onto another. The agent never
 * needs to know about this; it just sends flat controls.
 */

import type { ControlDef, VeldControls, BoundedNumericControlDef } from "../shared/controls";
import { isNumericControl, controlToAxis } from "../shared/controls";
import { attachScrub } from "../shared/number-scrub";
import { createXYPad } from "./xy-pad";
import { PREFIX } from "./constants";
import { api } from "./api";
import { toast } from "./toast";

/**
 * Parse controls from a message's `controls` field.
 * Returns null if the message has no controls.
 */
export function parseControls(message: { body: string; controls?: unknown[] }): ControlDef[] | null {
  if (message.controls && Array.isArray(message.controls) && message.controls.length > 0) {
    return message.controls as ControlDef[];
  }
  return null;
}

/** Per-row state for numeric controls that can be fused. */
interface NumericRowState {
  ctrl: BoundedNumericControlDef;
  row: HTMLElement;
  index: number;
}

/**
 * Render a set of controls as DOM elements.
 * Returns the container element and a cleanup function.
 */
export function renderControls(
  controls: ControlDef[],
  registry: VeldControls,
  threadId: string,
  options?: { inactive?: boolean },
): { element: HTMLElement; cleanup: () => void } {
  const inactive = options?.inactive ?? false;
  const cleanups: (() => void)[] = [];
  const container = document.createElement("div");
  container.className = PREFIX + "controls" + (inactive ? " " + PREFIX + "controls-inactive" : "");

  if (!Array.isArray(controls) || controls.length === 0) {
    return { element: container, cleanup: () => {} };
  }

  // Track numeric rows for fusion
  const numericRows: NumericRowState[] = [];
  // Track fused pairs so we can split them
  const fusedPairs: Map<string, { xIdx: number; yIdx: number; padEl: HTMLElement; padCleanup: () => void }> = new Map();

  for (let i = 0; i < controls.length; i++) {
    const ctrl = controls[i];

    // Validate required fields per control type
    if (ctrl.type === "select" && !Array.isArray(ctrl.options)) {
      console.warn(`[veld] Invalid select control "${ctrl.name}": missing options array`);
      continue;
    }

    const row = document.createElement("div");
    row.className = PREFIX + "control-row";
    row.dataset.controlIndex = String(i);

    switch (ctrl.type) {
      case "number":
      case "slider": {
        // Label row with fuse grip
        const labelRow = document.createElement("div");
        labelRow.className = PREFIX + "control-label-row";

        const label = document.createElement("label");
        label.className = PREFIX + "control-label";
        label.textContent = ctrl.label || ctrl.name;
        labelRow.appendChild(label);

        // Fuse grip — visible only on numeric controls with min/max
        if (isNumericControl(ctrl)) {
          const grip = document.createElement("span");
          grip.className = PREFIX + "control-fuse-grip";
          grip.title = "Drag onto another numeric control to create XY pad";
          grip.textContent = "\u2725"; // ✥ four-pointed star
          grip.draggable = true;

          const onDragStart = (e: DragEvent): void => {
            e.dataTransfer?.setData("application/x-veld-control", String(i));
            e.dataTransfer?.setData("text/plain", String(i));
            row.classList.add(PREFIX + "control-drag-source");
          };
          const onDragEnd = (): void => {
            row.classList.remove(PREFIX + "control-drag-source");
          };
          grip.addEventListener("dragstart", onDragStart);
          grip.addEventListener("dragend", onDragEnd);
          cleanups.push(() => {
            grip.removeEventListener("dragstart", onDragStart);
            grip.removeEventListener("dragend", onDragEnd);
          });

          labelRow.appendChild(grip);

          numericRows.push({ ctrl, row, index: i });
        }

        row.appendChild(labelRow);

        // Value display
        const valueDisplay = document.createElement("span");
        valueDisplay.className = PREFIX + "control-value";
        valueDisplay.textContent = String(ctrl.value) + (ctrl.unit ? " " + ctrl.unit : "");

        if (ctrl.type === "slider") {
          const input = document.createElement("input");
          input.type = "range";
          input.className = PREFIX + "control-slider";
          input.min = String(ctrl.min);
          input.max = String(ctrl.max);
          input.step = String(ctrl.step || 1);
          input.value = String(ctrl.value);
          registry.set(ctrl.name, ctrl.value);

          input.addEventListener("input", () => {
            const v = parseFloat(input.value);
            registry.set(ctrl.name, v);
            valueDisplay.textContent = String(v) + (ctrl.unit ? " " + ctrl.unit : "");
          });

          const inputRow = document.createElement("div");
          inputRow.style.cssText = "display:flex;align-items:center;gap:8px";
          inputRow.appendChild(input);
          inputRow.appendChild(valueDisplay);
          row.appendChild(inputRow);
        } else {
          const input = document.createElement("input");
          input.type = "number";
          input.className = PREFIX + "control-number";
          if (ctrl.min !== undefined) input.min = String(ctrl.min);
          if (ctrl.max !== undefined) input.max = String(ctrl.max);
          if (ctrl.step !== undefined) input.step = String(ctrl.step);
          input.value = String(ctrl.value);
          input.placeholder = "Alt+drag to scrub";
          registry.set(ctrl.name, ctrl.value);

          input.addEventListener("input", () => {
            const v = parseFloat(input.value);
            if (!isNaN(v)) {
              registry.set(ctrl.name, v);
              valueDisplay.textContent = String(v) + (ctrl.unit ? " " + ctrl.unit : "");
            }
          });

          const scrubCleanup = attachScrub(
            input,
            { min: ctrl.min, max: ctrl.max, step: ctrl.step || 1 },
            (v) => {
              registry.set(ctrl.name, v);
              valueDisplay.textContent = String(v) + (ctrl.unit ? " " + ctrl.unit : "");
            },
          );
          cleanups.push(scrubCleanup);

          const numRow = document.createElement("div");
          numRow.style.cssText = "display:flex;align-items:center;gap:8px";
          numRow.appendChild(input);
          numRow.appendChild(valueDisplay);
          row.appendChild(numRow);
        }

        // Drop target for fusion
        if (isNumericControl(ctrl)) {
          const onDragOver = (e: DragEvent): void => {
            if (!e.dataTransfer?.types.includes("application/x-veld-control")) return;
            e.preventDefault();
            row.classList.add(PREFIX + "control-drop-target");
          };
          const onDragLeave = (): void => {
            row.classList.remove(PREFIX + "control-drop-target");
          };
          const onDrop = (e: DragEvent): void => {
            e.preventDefault();
            row.classList.remove(PREFIX + "control-drop-target");
            const sourceIdx = parseInt(e.dataTransfer?.getData("text/plain") || "", 10);
            if (isNaN(sourceIdx) || sourceIdx === i) return;
            fuseControls(sourceIdx, i);
          };
          row.addEventListener("dragover", onDragOver);
          row.addEventListener("dragleave", onDragLeave);
          row.addEventListener("drop", onDrop);
          cleanups.push(() => {
            row.removeEventListener("dragover", onDragOver);
            row.removeEventListener("dragleave", onDragLeave);
            row.removeEventListener("drop", onDrop);
          });
        }
        break;
      }

      case "select": {
        const label = document.createElement("label");
        label.className = PREFIX + "control-label";
        label.textContent = ctrl.label || ctrl.name;
        row.appendChild(label);

        const select = document.createElement("select");
        select.className = PREFIX + "control-select";
        for (const opt of ctrl.options) {
          const option = document.createElement("option");
          option.value = opt;
          option.textContent = opt;
          if (opt === ctrl.value) option.selected = true;
          select.appendChild(option);
        }
        registry.set(ctrl.name, ctrl.value);

        select.addEventListener("change", () => {
          registry.set(ctrl.name, select.value);
        });
        row.appendChild(select);
        break;
      }

      case "color": {
        const label = document.createElement("label");
        label.className = PREFIX + "control-label";
        label.textContent = ctrl.label || ctrl.name;
        row.appendChild(label);

        const input = document.createElement("input");
        input.type = "color";
        input.className = PREFIX + "control-color";
        input.value = ctrl.value;
        registry.set(ctrl.name, ctrl.value);

        input.addEventListener("input", () => {
          registry.set(ctrl.name, input.value);
        });
        row.appendChild(input);
        break;
      }

      case "text": {
        const label = document.createElement("label");
        label.className = PREFIX + "control-label";
        label.textContent = ctrl.label || ctrl.name;
        row.appendChild(label);

        const input = document.createElement("input");
        input.type = "text";
        input.className = PREFIX + "control-text";
        input.value = ctrl.value;
        if (ctrl.placeholder) input.placeholder = ctrl.placeholder;
        registry.set(ctrl.name, ctrl.value);

        input.addEventListener("input", () => {
          registry.set(ctrl.name, input.value);
        });
        row.appendChild(input);
        break;
      }

      case "toggle": {
        const label = document.createElement("label");
        label.className = PREFIX + "control-toggle-label";

        const input = document.createElement("input");
        input.type = "checkbox";
        input.className = PREFIX + "control-toggle";
        input.checked = ctrl.value;
        registry.set(ctrl.name, ctrl.value);

        input.addEventListener("change", () => {
          registry.set(ctrl.name, input.checked);
        });

        label.appendChild(input);
        label.appendChild(document.createTextNode(" " + (ctrl.label || ctrl.name)));
        row.appendChild(label);
        break;
      }

      case "button": {
        const btn = document.createElement("button");
        btn.className = PREFIX + "control-button";
        btn.textContent = ctrl.label;
        btn.addEventListener("click", () => {
          registry.trigger(ctrl.name);
        });
        row.appendChild(btn);
        break;
      }
    }

    container.appendChild(row);
  }

  /** Fuse two numeric controls into an XY pad. */
  function fuseControls(xIdx: number, yIdx: number): void {
    const xCtrl = controls[xIdx];
    const yCtrl = controls[yIdx];
    if (!isNumericControl(xCtrl) || !isNumericControl(yCtrl)) return;

    // Normalize key so 0:1 and 1:0 are the same pair (#4)
    const key = Math.min(xIdx, yIdx) + ":" + Math.max(xIdx, yIdx);
    if (fusedPairs.has(key)) return;

    // Hide the original rows
    const xRow = container.querySelector(`[data-control-index="${xIdx}"]`) as HTMLElement | null;
    const yRow = container.querySelector(`[data-control-index="${yIdx}"]`) as HTMLElement | null;
    if (xRow) xRow.style.display = "none";
    if (yRow) yRow.style.display = "none";

    const { element, cleanup } = createXYPad(
      controlToAxis(xCtrl),
      controlToAxis(yCtrl),
      registry,
      () => splitControls(key),
    );

    // Insert the pad where the first control was
    const insertBefore = xRow?.nextSibling || yRow;
    if (insertBefore) {
      container.insertBefore(element, insertBefore);
    } else {
      // Insert before the Apply row
      const applyRow = container.querySelector("." + PREFIX + "control-apply-row");
      container.insertBefore(element, applyRow);
    }

    fusedPairs.set(key, { xIdx, yIdx, padEl: element, padCleanup: cleanup });
    cleanups.push(cleanup);
  }

  /** Split a fused XY pad back into individual controls. */
  function splitControls(key: string): void {
    const pair = fusedPairs.get(key);
    if (!pair) return;

    pair.padCleanup();
    pair.padEl.remove();
    fusedPairs.delete(key);

    // Remove from cleanups array to prevent double-cleanup (#1)
    const idx = cleanups.indexOf(pair.padCleanup);
    if (idx >= 0) cleanups.splice(idx, 1);

    // Show the original rows again
    const xRow = container.querySelector(`[data-control-index="${pair.xIdx}"]`) as HTMLElement | null;
    const yRow = container.querySelector(`[data-control-index="${pair.yIdx}"]`) as HTMLElement | null;
    if (xRow) xRow.style.display = "";
    if (yRow) yRow.style.display = "";
  }

  // Apply button — sends all values back to the agent
  const applyRow = document.createElement("div");
  applyRow.className = PREFIX + "control-row " + PREFIX + "control-apply-row";
  const applyBtn = document.createElement("button");
  applyBtn.className = PREFIX + "btn " + PREFIX + "btn-primary " + PREFIX + "btn-sm";
  applyBtn.textContent = "Apply values";
  applyBtn.addEventListener("click", () => {
    const values = registry.values();
    const body = "Applied values: " + JSON.stringify(values);
    api("POST", "/threads/" + threadId + "/messages", { body })
      .then(() => { toast("Values sent to agent"); })
      .catch(() => { toast("Failed to send values", true); });
  });
  applyRow.appendChild(applyBtn);
  container.appendChild(applyRow);

  return {
    element: container,
    cleanup: () => { cleanups.forEach((fn) => fn()); },
  };
}
