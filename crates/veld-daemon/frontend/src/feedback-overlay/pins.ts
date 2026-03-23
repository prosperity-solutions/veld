import { getState, dispatch } from "./store";
import {
  mkEl,
  appendGuarded,
  isCurrentPage,
  getThreadPageUrl,
  getThreadPosition,
  hasUnread,
} from "./helpers";
import { PREFIX, ICONS } from "./constants";
import { deps } from "../shared/registry";
import type { Thread } from "./types";

export function addPin(thread: Thread): void {
  if (thread.status === "resolved") return;
  const pageUrl = getThreadPageUrl(thread);
  if (!pageUrl || !isCurrentPage(pageUrl)) return;
  const pos = getThreadPosition(thread);
  if (!pos) return;

  removePin(thread.id);

  const pin = mkEl("div", "pin");
  pin.id = PREFIX + "pin-" + thread.id;
  pin.dataset.threadId = thread.id;

  const icon = mkEl("span", "pin-icon");
  icon.innerHTML = ICONS.chat;
  pin.appendChild(icon);

  const msgCount = thread.messages ? thread.messages.length : 1;
  if (msgCount > 1) {
    const count = mkEl("span", "pin-count", String(msgCount));
    pin.appendChild(count);
  }

  if (hasUnread(thread, getState().lastSeenAt)) {
    const dot = mkEl("span", "pin-unread-dot");
    pin.appendChild(dot);
  }

  pin.style.position = "absolute";
  pin.style.top = pos.y - 12 + "px";
  pin.style.left = pos.x + pos.width - 12 + "px";
  pin.style.zIndex = "calc(var(--vf-z) - 1)";

  pin.addEventListener("click", function (e: MouseEvent) {
    e.stopPropagation();
    deps().openThreadInPanel(thread.id);
  });

  appendGuarded(document.body, pin);
  dispatch({ type: "SET_PIN", threadId: thread.id, el: pin });
}

export function removePin(threadId: string): void {
  if (getState().pins[threadId]) {
    getState().pins[threadId].remove();
    dispatch({ type: "REMOVE_PIN", threadId });
  }
}

export function renderAllPins(): void {
  Object.keys(getState().pins).forEach(removePin);
  getState().threads.forEach(function (t: Thread) {
    if (t.status === "open") addPin(t);
  });
}

export function repositionPins(): void {
  getState().threads.forEach(function (t: Thread) {
    const pin = getState().pins[t.id];
    if (!pin) return;
    if (!t.scope || t.scope.type !== "element" || !t.scope.selector) return;
    try {
      const el = document.querySelector(t.scope.selector);
      if (el) {
        const vr = el.getBoundingClientRect();
        if (vr.width === 0 && vr.height === 0) {
          pin.style.display = "none";
          return;
        }
        pin.style.display = "";
        const r = {
          x: vr.left + window.scrollX,
          y: vr.top + window.scrollY,
          width: vr.width,
          height: vr.height,
        };
        t.scope.position = r;
        pin.style.top = r.y - 12 + "px";
        pin.style.left = r.x + r.width - 12 + "px";
      } else {
        pin.style.display = "none";
      }
    } catch (_) {}
  });
}

export function scheduleReposition(): void {
  if (getState().rafPending) return;
  dispatch({ type: "SET_RAF_PENDING", pending: true });
  requestAnimationFrame(function () {
    dispatch({ type: "SET_RAF_PENDING", pending: false });
    repositionPins();
  });
}
