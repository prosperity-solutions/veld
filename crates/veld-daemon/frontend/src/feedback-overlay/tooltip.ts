import { refs } from "./refs";
import { mkEl } from "./helpers";
import { PREFIX } from "./constants";

let suppressed = false;

/** Suppress tooltip display (e.g. during FAB drag). */
export function suppressTooltip(suppress: boolean): void {
  suppressed = suppress;
  if (suppress) hideTooltip();
}

/** Create and attach the tooltip element to the shadow DOM. */
export function initTooltip(): void {
  refs.tooltip = mkEl("div", "tooltip");
  refs.shadow.appendChild(refs.tooltip);
}

/** Show the tooltip above (or below) the given anchor element. */
export function showTooltip(anchor: Element, html: string): void {
  if (suppressed) return;
  refs.tooltip.innerHTML = html;
  refs.tooltip.style.display = "block";
  const r = anchor.getBoundingClientRect();
  const tw = refs.tooltip.offsetWidth;
  const th = refs.tooltip.offsetHeight;
  const gap = 8;
  // Prefer above
  let top = r.top + window.scrollY - th - gap;
  if (top < window.scrollY + 4) {
    top = r.bottom + window.scrollY + gap; // flip below
  }
  let left = r.left + window.scrollX + r.width / 2 - tw / 2;
  left = Math.max(
    window.scrollX + 4,
    Math.min(window.scrollX + window.innerWidth - tw - 4, left),
  );
  refs.tooltip.style.top = top + "px";
  refs.tooltip.style.left = left + "px";
}

/** Hide the tooltip. */
export function hideTooltip(): void {
  refs.tooltip.style.display = "none";
}

/** Build tooltip HTML. `keys` is an array of individual key labels, e.g. [KEY_MOD, KEY_SHIFT, "F"]. */
export function tipHtml(label: string, keys?: string[]): string {
  let h = label;
  if (keys && keys.length) {
    h += ' <span class="' + PREFIX + 'kbd-group">';
    for (let i = 0; i < keys.length; i++) {
      h += '<kbd class="' + PREFIX + 'kbd">' + keys[i] + "</kbd>";
    }
    h += "</span>";
  }
  return h;
}

/** Attach tooltip show/hide listeners to an element. */
export function attachTooltip(el: HTMLElement, html: string): void {
  el.addEventListener("mouseenter", () => {
    showTooltip(el, html);
  });
  el.addEventListener("mouseleave", hideTooltip);
  el.addEventListener("mousedown", hideTooltip);
}

/** Position an element (tooltip/popover) above or below a viewport rect. */
export function positionTooltip(
  el: HTMLElement,
  viewportRect: { top: number; left: number; width: number; height: number },
): void {
  const gap = 6;
  const margin = 8;
  const aboveY =
    viewportRect.top + window.scrollY - el.offsetHeight - gap;
  const belowY =
    viewportRect.top + window.scrollY + viewportRect.height + gap;
  // Prefer above, flip below if it would go off-screen
  if (aboveY < window.scrollY + margin) {
    el.style.top = belowY + "px";
  } else {
    el.style.top = aboveY + "px";
  }
  const left = viewportRect.left + window.scrollX;
  const maxLeft =
    window.scrollX + window.innerWidth - el.offsetWidth - margin;
  el.style.left =
    Math.max(window.scrollX + margin, Math.min(maxLeft, left)) + "px";
}
