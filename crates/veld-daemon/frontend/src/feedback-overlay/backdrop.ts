// @ts-nocheck
import { S } from "./state";
import { PREFIX } from "./constants";
import { docRect, selectorFor, formatTrace } from "./helpers";
import { getComponentTrace } from "./component-trace";

// These are set by init to avoid circular imports
export let captureScreenshotFn: (x: number, y: number, w: number, h: number) => void;
export let showCreatePopoverFn: (rect: any, selector: string, tagInfo: string, targetEl: Element, trace: string[] | null) => void;
export let positionTooltipFn: (el: HTMLElement, viewportRect: DOMRect) => void;

export function setBackdropDeps(deps: {
  captureScreenshot: typeof captureScreenshotFn;
  showCreatePopover: typeof showCreatePopoverFn;
  positionTooltip: typeof positionTooltipFn;
}) {
  captureScreenshotFn = deps.captureScreenshot;
  showCreatePopoverFn = deps.showCreatePopover;
  positionTooltipFn = deps.positionTooltip;
}

export function elementBelowBackdrop(x: number, y: number): Element | null {
  S.overlay.style.display = "none";
  S.hoverOutline.style.display = "none";
  S.componentTraceEl.style.display = "none";
  var el = document.elementFromPoint(x, y);
  S.overlay.style.display = "";
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
  var ssStartX: number, ssStartY: number, ssDragging = false;

  S.overlay.addEventListener("mousemove", function (e: MouseEvent) {
    if (S.activeMode === "select-element") {
      if (S.lockedEl) return;
      var target = elementBelowBackdrop(e.clientX, e.clientY);
      if (!target) {
        S.hoverOutline.style.display = "none";
        S.componentTraceEl.style.display = "none";
        S.hoveredEl = null;
        return;
      }
      S.hoveredEl = target;
      var r = target.getBoundingClientRect();
      S.hoverOutline.style.display = "block";
      S.hoverOutline.style.top = (r.top + window.scrollY) + "px";
      S.hoverOutline.style.left = (r.left + window.scrollX) + "px";
      S.hoverOutline.style.width = r.width + "px";
      S.hoverOutline.style.height = r.height + "px";

      var trace = getComponentTrace(target);
      if (trace && trace.length) {
        S.componentTraceEl.textContent = formatTrace(trace);
        S.componentTraceEl.style.display = "block";
        positionTooltipFn(S.componentTraceEl, r as DOMRect);
      } else {
        S.componentTraceEl.style.display = "none";
      }
    } else if (S.activeMode === "screenshot" && ssDragging) {
      var x = Math.min(ssStartX, e.clientX);
      var y = Math.min(ssStartY, e.clientY);
      var w = Math.abs(e.clientX - ssStartX);
      var h = Math.abs(e.clientY - ssStartY);
      S.screenshotRect.style.display = "block";
      S.screenshotRect.style.left = (x + window.scrollX) + "px";
      S.screenshotRect.style.top = (y + window.scrollY) + "px";
      S.screenshotRect.style.width = w + "px";
      S.screenshotRect.style.height = h + "px";
    }
  });

  S.overlay.addEventListener("mousedown", function (e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (S.activeMode === "screenshot") {
      ssDragging = true;
      ssStartX = e.clientX;
      ssStartY = e.clientY;
      S.screenshotRect.style.display = "none";
    }
  });

  S.overlay.addEventListener("mouseup", function (e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (S.activeMode === "screenshot" && ssDragging) {
      ssDragging = false;
      var x = Math.min(ssStartX, e.clientX);
      var y = Math.min(ssStartY, e.clientY);
      var w = Math.abs(e.clientX - ssStartX);
      var h = Math.abs(e.clientY - ssStartY);
      S.screenshotRect.style.display = "none";
      if (w > 10 && h > 10) {
        captureScreenshotFn(x, y, w, h);
      }
    }
  });

  S.overlay.addEventListener("click", function (e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (S.activeMode === "select-element") {
      var target = S.hoveredEl || elementBelowBackdrop(e.clientX, e.clientY);
      if (!target) return;
      var rect = docRect(target);
      var selector = selectorFor(target);
      var tagInfo = target.tagName.toLowerCase();
      if (target.className && typeof target.className === "string") {
        var cls = target.className.trim().split(/\s+/).filter(function (c: string) { return !c.startsWith(PREFIX); });
        if (cls.length) tagInfo += "." + cls.slice(0, 3).join(".");
      }
      var trace = getComponentTrace(target);
      showCreatePopoverFn(rect, selector, tagInfo, target, trace);
    }
  });
}
