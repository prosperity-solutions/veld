import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { Thread } from "./types";
import { mkEl, submitOnModEnter, formatTrace } from "./helpers";
import { PREFIX, SUBMIT_HINT } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { deps } from "../shared/registry";

export function positionPopover(
  pop: HTMLElement,
  anchorRect: { x: number; y: number; width: number; height: number },
): void {
  const popHeight = 260;
  const gap = 10;
  const margin = 16;

  const topBelow = anchorRect.y + anchorRect.height + gap;
  const topAbove = anchorRect.y - popHeight - gap;

  let top: number;
  if (topBelow + popHeight > window.scrollY + window.innerHeight - margin && topAbove > window.scrollY + margin) {
    top = topAbove;
  } else {
    top = topBelow;
  }

  let left = anchorRect.x + anchorRect.width / 2 - 180;
  const maxLeft = window.scrollX + window.innerWidth - 360 - margin;
  const minLeft = window.scrollX + margin;
  left = Math.max(minLeft, Math.min(maxLeft, left));

  pop.style.top = top + "px";
  pop.style.left = left + "px";
}

export function closeActivePopover(): void {
  const popover = getState().activePopover;
  if (popover) {
    if (typeof popover._veldCleanup === "function") {
      popover._veldCleanup();
    }
    popover.remove();
    dispatch({ type: "SET_POPOVER", popover: null });
  }
  if (getState().lockedEl) {
    dispatch({ type: "SET_LOCKED", el: null });
    refs.hoverOutline.style.display = "none";
    refs.componentTraceEl.style.display = "none";
  }
  if (refs.toolBtnPageComment) refs.toolBtnPageComment.classList.remove(PREFIX + "tool-active");
  if (refs.toolBtnScreenshot) refs.toolBtnScreenshot.classList.remove(PREFIX + "tool-active");
}

export function showCreatePopover(
  rect: { x: number; y: number; width: number; height: number },
  selector: string | null,
  tagInfo: string | null,
  targetEl: Element | null,
  trace: string[] | null,
): void {
  closeActivePopover();
  dispatch({ type: "SET_LOCKED", el: targetEl });

  const popover = mkEl("div", "popover");

  if (selector) popover.appendChild(mkEl("div", "popover-selector", selector));
  if (trace) {
    const formatted = formatTrace(trace);
    if (formatted) popover.appendChild(mkEl("div", "popover-trace", formatted));
  }

  const popoverBody = mkEl("div", "popover-body");

  const textarea = document.createElement("textarea");
  textarea.className = PREFIX + "textarea";
  textarea.placeholder = "Leave feedback...";
  textarea.rows = 3;
  popoverBody.appendChild(textarea);

  const actions = mkEl("div", "popover-actions");
  const cancelBtn = mkEl("button", "btn btn-secondary btn-sm", "Cancel");
  cancelBtn.addEventListener("click", closeActivePopover);
  actions.appendChild(cancelBtn);

  const sendBtn = mkEl("button", "btn btn-primary btn-sm", "Send" + SUBMIT_HINT) as HTMLButtonElement;
  sendBtn.addEventListener("click", () => {
    const text = (textarea as HTMLTextAreaElement).value.trim();
    if (!text || sendBtn.disabled) return;
    sendBtn.disabled = true;
    const scope = selector
      ? { type: "element", page_url: window.location.pathname, selector, position: rect }
      : { type: "page", page_url: window.location.pathname };
    api("POST", "/threads", {
      scope, message: text, component_trace: trace || null,
      viewport_width: window.innerWidth, viewport_height: window.innerHeight,
    }).then((raw) => {
      const thread = raw as Thread;
      dispatch({ type: "ADD_THREAD", thread });
      closeActivePopover();
      deps().addPin(thread);
      deps().updateBadge();
      if (getState().panelOpen) deps().renderPanel();
      toast("Thread created");
    }).catch(() => {
      sendBtn.disabled = false;
      toast("Failed to create thread", true);
    });
  });
  actions.appendChild(sendBtn);
  submitOnModEnter(textarea, sendBtn);
  popoverBody.appendChild(actions);
  popover.appendChild(popoverBody);

  refs.shadow.appendChild(popover);
  dispatch({ type: "SET_POPOVER", popover });
  positionPopover(popover, rect);
  textarea.focus();
}

export function togglePageComment(): void {
  if (getState().activePopover) { closeActivePopover(); return; }
  showCreatePopover(
    { x: window.innerWidth / 2 - 180 + window.scrollX, y: 120 + window.scrollY, width: 0, height: 0 },
    null, null, null, null,
  );
  refs.toolBtnPageComment.classList.add(PREFIX + "tool-active");
}
