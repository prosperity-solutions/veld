/**
 * Renders interactive controls from agent messages into the thread panel.
 *
 * When an agent sends a message with a `controls` field, this module
 * renders sliders, number inputs, dropdowns, color pickers, toggles,
 * and buttons — all wired to window.__veld_controls.
 */

import type { ControlDef, VeldControls } from "../shared/controls";
import { attachScrub } from "../shared/number-scrub";
import { PREFIX } from "./constants";
import { api } from "./api";
import { toast } from "./toast";

/**
 * Parse controls from a message body.
 * Controls are embedded as a JSON block after <!--veld-controls--> marker,
 * or as a `controls` property on the message object.
 */
export function parseControls(message: { body: string; controls?: ControlDef[] }): ControlDef[] | null {
  if (message.controls && Array.isArray(message.controls)) {
    return message.controls;
  }
  // Try parsing from body marker
  const marker = "<!--veld-controls-->";
  const idx = message.body.indexOf(marker);
  if (idx >= 0) {
    try {
      const json = message.body.substring(idx + marker.length).trim();
      const parsed = JSON.parse(json);
      if (Array.isArray(parsed)) {
        return parsed;
      }
      if (parsed.controls && Array.isArray(parsed.controls)) {
        return parsed.controls;
      }
    } catch { /* ignore parse errors */ }
  }
  return null;
}

/**
 * Render a set of controls as DOM elements.
 * Returns the container element and a cleanup function.
 */
export function renderControls(
  controls: ControlDef[],
  registry: VeldControls,
  threadId: string,
): { element: HTMLElement; cleanup: () => void } {
  const cleanups: (() => void)[] = [];
  const container = document.createElement("div");
  container.className = PREFIX + "controls";

  for (const ctrl of controls) {
    const row = document.createElement("div");
    row.className = PREFIX + "control-row";

    switch (ctrl.type) {
      case "number":
      case "slider": {
        // Label
        const label = document.createElement("label");
        label.className = PREFIX + "control-label";
        label.textContent = ctrl.label || ctrl.name;
        row.appendChild(label);

        // Value display
        const valueDisplay = document.createElement("span");
        valueDisplay.className = PREFIX + "control-value";
        valueDisplay.textContent = String(ctrl.value) + (ctrl.unit ? " " + ctrl.unit : "");

        if (ctrl.type === "slider") {
          // Range slider
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

          row.appendChild(input);
          row.appendChild(valueDisplay);
        } else {
          // Number input with Bret Victor scrubbing
          const input = document.createElement("input");
          input.type = "number";
          input.className = PREFIX + "control-number";
          if (ctrl.min !== undefined) input.min = String(ctrl.min);
          if (ctrl.max !== undefined) input.max = String(ctrl.max);
          if (ctrl.step !== undefined) input.step = String(ctrl.step);
          input.value = String(ctrl.value);
          registry.set(ctrl.name, ctrl.value);

          input.addEventListener("input", () => {
            const v = parseFloat(input.value);
            if (!isNaN(v)) {
              registry.set(ctrl.name, v);
              valueDisplay.textContent = String(v) + (ctrl.unit ? " " + ctrl.unit : "");
            }
          });

          // Attach Bret Victor scrubbing
          const scrubCleanup = attachScrub(
            input,
            { min: ctrl.min, max: ctrl.max, step: ctrl.step || 1 },
            (v) => {
              registry.set(ctrl.name, v);
              valueDisplay.textContent = String(v) + (ctrl.unit ? " " + ctrl.unit : "");
            },
          );
          cleanups.push(scrubCleanup);

          row.appendChild(input);
          row.appendChild(valueDisplay);
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
