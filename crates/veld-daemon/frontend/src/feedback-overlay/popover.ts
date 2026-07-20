import { refs } from "./refs";
import { getState, dispatch } from "./store";
import type { Thread, VeldPopoverElement } from "./types";
import { mkEl, submitOnModEnter, formatTrace, docRect } from "./helpers";
import { PREFIX, SUBMIT_HINT, ICONS } from "./constants";
import { api } from "./api";
import { toast } from "./toast";
import { deps } from "../shared/registry";
import {
  saveComposerDraft,
  clearComposerDraft,
  pageKey,
  type ComposerDraft,
} from "./persist";

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

/** Append a top-right close (×) button to a popover — a clear, window-style
 *  dismiss affordance next to Escape/Cancel. Call this on any manually-built
 *  popover; `showCreatePopover` already does, as does the screenshot editor. */
export function appendPopoverClose(pop: HTMLElement): void {
  const close = mkEl("button", "popover-close");
  close.setAttribute("aria-label", "Close");
  close.innerHTML = ICONS.cancel;
  close.addEventListener("click", (e) => {
    e.stopPropagation();
    closeActivePopover();
  });
  pop.appendChild(close);
}

export function closeActivePopover(): void {
  // The composer is going away (sent, cancelled, ×, Escape, a thread action
  // that closes it, or replaced by a new selection) — its unsent draft must
  // not survive into the next reload. Clear against the page the composer was
  // opened on (`_veldPageKey`), which may differ from the current URL if the
  // app client-navigated while the composer stayed open.
  const popover = getState().activePopover;
  clearComposerDraft(popover?._veldPageKey);
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

export interface CreatePopoverExtra {
  elementText?: string | null;
  sourceFile?: string | null;
  sourceLine?: number | null;
}

export function showCreatePopover(
  rect: { x: number; y: number; width: number; height: number },
  selector: string | null,
  tagInfo: string | null,
  targetEl: Element | null,
  trace: string[] | null,
  extra?: CreatePopoverExtra | null,
): void {
  closeActivePopover();
  dispatch({ type: "SET_LOCKED", el: targetEl });

  const popover = mkEl("div", "popover");

  if (selector) popover.appendChild(mkEl("div", "popover-selector", selector));
  if (trace) {
    const formatted = formatTrace(trace);
    if (formatted) popover.appendChild(mkEl("div", "popover-trace", formatted));
  }
  if (extra?.elementText) popover.appendChild(mkEl("div", "popover-selector", "“" + extra.elementText + "”"));

  const popoverBody = mkEl("div", "popover-body");

  const textarea = document.createElement("textarea");
  textarea.className = PREFIX + "textarea";
  textarea.placeholder = "Leave feedback...";
  textarea.rows = 3;
  popoverBody.appendChild(textarea);

  // Mirror the draft (text + the scope it's attached to) to tab-local storage
  // on every keystroke so a reload can re-open this exact composer. Writes are
  // synchronous and tiny; an empty box clears the draft rather than restoring
  // an empty popover on the next reload.
  //
  // The composer survives a client-side (SPA) navigation — onNavigate doesn't
  // close it — so pin the URL it opened on and stop persisting once the app
  // routes elsewhere: the draft's selector/rect are relative to the open-time
  // page and must not be written under a different URL's key (which a later
  // reload of that URL would then mis-restore).
  const openPageKey = pageKey();
  (popover as VeldPopoverElement)._veldPageKey = openPageKey;
  textarea.addEventListener("input", () => {
    if (pageKey() !== openPageKey) return;
    if (textarea.value.trim()) {
      saveComposerDraft({
        text: textarea.value,
        isPage: !selector,
        selector,
        tagInfo,
        trace,
        elementText: extra?.elementText ?? null,
        sourceFile: extra?.sourceFile ?? null,
        sourceLine: extra?.sourceLine ?? null,
        rect,
      });
    } else {
      clearComposerDraft();
    }
  });

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
      ? {
          type: "element",
          page_url: window.location.pathname,
          selector,
          position: rect,
          element_text: extra?.elementText || undefined,
          source_file: extra?.sourceFile || undefined,
          source_line: extra?.sourceLine ?? undefined,
        }
      : { type: "page", page_url: window.location.pathname };
    api("POST", "/threads", {
      scope, message: text, component_trace: trace || null,
      viewport_width: window.innerWidth, viewport_height: window.innerHeight,
    }).then((raw) => {
      const thread = raw as Thread;
      dispatch({ type: "ADD_THREAD", thread });
      closeActivePopover();
      deps().setMode(null);
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
  appendPopoverClose(popover);

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

/** Fallback anchor: viewport-centred, current scroll — used when a restored
 *  draft's element can't be re-found so the popover still lands somewhere
 *  visible. */
function fallbackRect(): { x: number; y: number; width: number; height: number } {
  return {
    x: window.scrollX + window.innerWidth / 2 - 180,
    y: window.scrollY + 120,
    width: 0,
    height: 0,
  };
}

/** Re-open the new-comment composer from a persisted draft after a reload.
 *
 *  Element-scoped drafts are re-anchored to the live element by selector (the
 *  same lookup navigation.ts uses to scroll to a thread). If the element is
 *  gone — removed or renamed by the edit that triggered the reload — the draft
 *  and its original element scope are KEPT (so the thread still attaches to
 *  that selector on send, degrading like an anchored pin whose element left),
 *  and the popover is shown at a visible fallback position instead of the
 *  vanished element's stale coordinates. */
export function restoreComposer(draft: ComposerDraft): void {
  const finiteRect = (r: { x: number; y: number; width: number; height: number }): boolean =>
    !!r && [r.x, r.y, r.width, r.height].every((n) => typeof n === "number" && isFinite(n));

  // Page/global comments aren't anchored to anything, so their saved rect is
  // just an arbitrary point captured at open time — and its document-relative
  // Y is stale after a reload that changed scroll or layout (exactly the HMR
  // case), which would drop the composer off-screen. Re-centre it in the
  // current viewport instead. Element comments re-anchor to the live element
  // below; only when that element is gone do they fall back too.
  let rect = draft.isPage || !finiteRect(draft.rect) ? fallbackRect() : draft.rect;
  let targetEl: Element | null = null;
  if (!draft.isPage && draft.selector) {
    try { targetEl = document.querySelector(draft.selector); } catch (_) { targetEl = null; }
    if (targetEl) {
      rect = docRect(targetEl); // re-anchor to the element's fresh position
    } else {
      rect = fallbackRect();
      toast("The element for your saved comment is gone — draft kept");
    }
  }

  showCreatePopover(
    rect,
    draft.isPage ? null : draft.selector,
    draft.tagInfo,
    targetEl,
    draft.trace,
    draft.isPage
      ? null
      : { elementText: draft.elementText, sourceFile: draft.sourceFile, sourceLine: draft.sourceLine },
  );

  // showCreatePopover cleared the persisted draft (via closeActivePopover) and
  // built an empty textarea; refill it and fire `input` so the listener above
  // re-persists — keeping the text through a second reload before the user
  // types again, and re-saving with the element's fresh anchor rect.
  const pop = getState().activePopover;
  const ta = pop
    ? (pop.querySelector("textarea." + PREFIX + "textarea") as HTMLTextAreaElement | null)
    : null;
  if (ta) {
    ta.value = draft.text;
    ta.dispatchEvent(new Event("input", { bubbles: true }));
    ta.focus();
    try { ta.setSelectionRange(draft.text.length, draft.text.length); } catch (_) { /* ignore */ }
  }
}
