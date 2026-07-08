import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { docRect, selectorFor, formatTrace, truncateMiddle } from "./helpers";
import { getComponentTrace, getComponentSource } from "./component-trace";
import { clampToFrame } from "./screenshot";
import { deps } from "../shared/registry";
import { toast } from "./toast";

export function elementBelowBackdrop(x: number, y: number): Element | null {
  refs.overlay.style.display = "none";
  refs.hoverOutline.style.display = "none";
  refs.componentTraceEl.style.display = "none";
  let el = document.elementFromPoint(x, y);
  refs.overlay.style.display = "";
  if (el && isOwnElement(el)) el = null;
  return el;
}

export function isOwnElement(el: Element | null): boolean {
  while (el) {
    if (el.className && typeof el.className === "string" && el.className.indexOf(PREFIX) !== -1) return true;
    el = el.parentElement;
  }
  return false;
}

export function initBackdropEvents(): void {
  let ssStartX: number, ssStartY: number, ssDragging = false;

  refs.overlay.addEventListener("mousemove", function (e: MouseEvent) {
    if (getState().activeMode === "select-element") {
      if (getState().lockedEl) return;
      const target = elementBelowBackdrop(e.clientX, e.clientY);
      if (!target) {
        refs.hoverOutline.style.display = "none";
        refs.componentTraceEl.style.display = "none";
        dispatch({ type: "SET_HOVERED", el: null });
        return;
      }
      dispatch({ type: "SET_HOVERED", el: target });
      const r = target.getBoundingClientRect();
      refs.hoverOutline.style.display = "block";
      refs.hoverOutline.style.top = (r.top + window.scrollY) + "px";
      refs.hoverOutline.style.left = (r.left + window.scrollX) + "px";
      refs.hoverOutline.style.width = r.width + "px";
      refs.hoverOutline.style.height = r.height + "px";

      const trace = getComponentTrace(target);
      if (trace && trace.length) {
        refs.componentTraceEl.textContent = formatTrace(trace) ?? "";
        refs.componentTraceEl.style.display = "block";
        deps().positionTooltip(refs.componentTraceEl, r);
      } else {
        refs.componentTraceEl.style.display = "none";
      }
    } else if (getState().activeMode === "screenshot" && ssDragging) {
      const rawX = Math.min(ssStartX, e.clientX);
      const rawY = Math.min(ssStartY, e.clientY);
      const rawW = Math.abs(e.clientX - ssStartX);
      const rawH = Math.abs(e.clientY - ssStartY);
      // Clamp to the displayed frame so a drag into a letterbox bar can't
      // select "outside the image".
      const { x, y, w, h } = clampToFrame(rawX, rawY, rawW, rawH);
      refs.screenshotRect.style.display = "block";
      refs.screenshotRect.style.left = (x + window.scrollX) + "px";
      refs.screenshotRect.style.top = (y + window.scrollY) + "px";
      refs.screenshotRect.style.width = w + "px";
      refs.screenshotRect.style.height = h + "px";
    }
  });

  refs.overlay.addEventListener("mousedown", function (e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (getState().activeMode === "screenshot") {
      ssDragging = true;
      ssStartX = e.clientX;
      ssStartY = e.clientY;
      refs.screenshotRect.style.display = "none";
    }
  });

  refs.overlay.addEventListener("mouseup", function (e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (getState().activeMode === "screenshot" && ssDragging) {
      ssDragging = false;
      const rawX = Math.min(ssStartX, e.clientX);
      const rawY = Math.min(ssStartY, e.clientY);
      const rawW = Math.abs(e.clientX - ssStartX);
      const rawH = Math.abs(e.clientY - ssStartY);
      const { x, y, w, h } = clampToFrame(rawX, rawY, rawW, rawH);
      refs.screenshotRect.style.display = "none";
      // Judge "was this a real drag" on the raw gesture, not the clamped
      // result — a deliberate drag that lands mostly in a letterbox bar
      // would otherwise clamp below the threshold and silently no-op.
      if (rawW <= 10 || rawH <= 10) return;
      if (w > 10 && h > 10) {
        deps().captureScreenshot(x, y, w, h);
      } else {
        toast("Selection was outside the captured frame", true);
      }
    }
  });

  refs.overlay.addEventListener("click", function (e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (getState().activeMode === "select-element") {
      const target = getState().hoveredEl || elementBelowBackdrop(e.clientX, e.clientY);
      if (!target) return;
      // Read innerText first — it forces a synchronous layout, so doing it
      // before the getBoundingClientRect()/fiber-walk calls below avoids
      // sandwiching a reflow between two other layout-touching reads.
      const rawText = (target as HTMLElement).innerText ?? target.textContent ?? "";
      const elementText = rawText.trim() ? truncateMiddle(rawText) : null;
      const rect = docRect(target);
      const selector = selectorFor(target);
      let tagInfo = target.tagName.toLowerCase();
      if (target.className && typeof target.className === "string") {
        const cls = target.className.trim().split(/\s+/).filter(function (c: string) { return !c.startsWith(PREFIX); });
        if (cls.length) tagInfo += "." + cls.slice(0, 3).join(".");
      }
      const trace = getComponentTrace(target);
      const source = getComponentSource(target);
      deps().showCreatePopover(rect, selector, tagInfo, target, trace, {
        elementText,
        sourceFile: source?.file,
        sourceLine: source?.line,
      });
    }
  });
}
