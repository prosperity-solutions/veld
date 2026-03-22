import { S } from "./state";
import { findThread, isCurrentPage, getThreadPageUrl } from "./helpers";
import { PREFIX } from "./constants";

const SCROLL_TO_KEY = "veld-feedback-scroll-to-thread";

// Late-bound deps
let renderAllPinsFn: () => void;
let renderPanelFn: () => void;

export function setNavigationDeps(deps: {
  renderAllPins: () => void;
  renderPanel: () => void;
}): void {
  renderAllPinsFn = deps.renderAllPins;
  renderPanelFn = deps.renderPanel;
}

export function scrollToThread(threadId: string): void {
  const thread = findThread(S.threads, threadId);
  if (!thread) return;

  const pageUrl = getThreadPageUrl(thread);
  if (pageUrl && !isCurrentPage(pageUrl)) {
    try { sessionStorage.setItem(SCROLL_TO_KEY, threadId); } catch (_) {}
    window.location.href = pageUrl;
    return;
  }

  let target: Element | null = null;
  if (thread.scope && thread.scope.type === "element" && thread.scope.selector) {
    try { target = document.querySelector(thread.scope.selector); } catch (_) {}
  }
  if (!target) target = S.pins[threadId] || document.getElementById(PREFIX + "pin-" + threadId);
  if (!target) return;

  target.scrollIntoView({ behavior: "smooth", block: "center" });

  const pin = S.pins[threadId];
  if (pin) {
    setTimeout(() => {
      pin.classList.remove(PREFIX + "pin-highlight");
      void pin.offsetWidth;
      pin.classList.add(PREFIX + "pin-highlight");
      setTimeout(() => { pin.classList.remove(PREFIX + "pin-highlight"); }, 1500);
    }, 400);
  }
}

export function checkPendingScroll(): void {
  try {
    const id = sessionStorage.getItem(SCROLL_TO_KEY);
    if (id) {
      sessionStorage.removeItem(SCROLL_TO_KEY);
      setTimeout(() => scrollToThread(id), 300);
    }
  } catch (_) {}
}

export function onNavigate(): void {
  const newPath = window.location.pathname;
  if (newPath !== S.lastPathname) {
    S.lastPathname = newPath;
    if (renderAllPinsFn) renderAllPinsFn();
    if (S.panelOpen && renderPanelFn) renderPanelFn();
    checkPendingScroll();
  }
}
