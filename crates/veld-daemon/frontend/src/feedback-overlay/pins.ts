import { S } from "./state";
import {
  mkEl,
  docRect,
  isCurrentPage,
  getThreadPageUrl,
  getThreadPosition,
  hasUnread,
} from "./helpers";
import { PREFIX, ICONS } from "./constants";
import type { Thread } from "./types";

/** Late-bound callback — set by the orchestrator once panel module is available. */
let _openThreadInPanel: ((threadId: string) => void) | null = null;

export function setOpenThreadInPanel(
  fn: (threadId: string) => void,
): void {
  _openThreadInPanel = fn;
}

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

  if (hasUnread(thread, S.lastSeenAt)) {
    const dot = mkEl("span", "pin-unread-dot");
    pin.appendChild(dot);
  }

  pin.style.position = "absolute";
  pin.style.top = pos.y - 12 + "px";
  pin.style.left = pos.x + pos.width - 12 + "px";
  pin.style.zIndex = "calc(var(--vf-z) - 1)";

  pin.addEventListener("click", function (e: MouseEvent) {
    e.stopPropagation();
    if (_openThreadInPanel) _openThreadInPanel(thread.id);
  });

  document.body.appendChild(pin);
  S.pins[thread.id] = pin;
}

export function removePin(threadId: string): void {
  if (S.pins[threadId]) {
    S.pins[threadId].remove();
    delete S.pins[threadId];
  }
}

export function renderAllPins(): void {
  Object.keys(S.pins).forEach(removePin);
  S.threads.forEach(function (t: Thread) {
    if (t.status === "open") addPin(t);
  });
}

export function repositionPins(): void {
  S.threads.forEach(function (t: Thread) {
    const pin = S.pins[t.id];
    if (!pin) return;
    if (!t.scope || t.scope.type !== "element" || !t.scope.selector) return;
    try {
      const el = document.querySelector(t.scope.selector);
      if (el) {
        const r = docRect(el);
        t.scope.position = {
          x: r.x,
          y: r.y,
          width: r.width,
          height: r.height,
        };
        pin.style.top = r.y - 12 + "px";
        pin.style.left = r.x + r.width - 12 + "px";
      }
    } catch (_) {}
  });
}

export function scheduleReposition(): void {
  if (S.rafPending) return;
  S.rafPending = true;
  requestAnimationFrame(function () {
    S.rafPending = false;
    repositionPins();
  });
}
