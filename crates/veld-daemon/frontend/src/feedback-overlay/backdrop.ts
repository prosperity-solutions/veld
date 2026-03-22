import { refs } from "./refs";
import { getState, dispatch } from "./store";
import { PREFIX } from "./constants";
import { docRect, selectorFor, formatTrace } from "./helpers";
import { getComponentTrace } from "./component-trace";
import { deps } from "../shared/registry";

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
      const x = Math.min(ssStartX, e.clientX);
      const y = Math.min(ssStartY, e.clientY);
      const w = Math.abs(e.clientX - ssStartX);
      const h = Math.abs(e.clientY - ssStartY);
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
      const x = Math.min(ssStartX, e.clientX);
      const y = Math.min(ssStartY, e.clientY);
      const w = Math.abs(e.clientX - ssStartX);
      const h = Math.abs(e.clientY - ssStartY);
      refs.screenshotRect.style.display = "none";
      if (w > 10 && h > 10) {
        deps().captureScreenshot(x, y, w, h);
      }
    }
  });

  refs.overlay.addEventListener("click", function (e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (getState().activeMode === "select-element") {
      const target = getState().hoveredEl || elementBelowBackdrop(e.clientX, e.clientY);
      if (!target) return;
      const rect = docRect(target);
      const selector = selectorFor(target);
      let tagInfo = target.tagName.toLowerCase();
      if (target.className && typeof target.className === "string") {
        const cls = target.className.trim().split(/\s+/).filter(function (c: string) { return !c.startsWith(PREFIX); });
        if (cls.length) tagInfo += "." + cls.slice(0, 3).join(".");
      }
      const trace = getComponentTrace(target);
      deps().showCreatePopover(rect, selector, tagInfo, target, trace);
    }
  });
}
