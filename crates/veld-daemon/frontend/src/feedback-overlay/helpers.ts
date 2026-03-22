import { PREFIX, IS_MAC } from "./constants";
import type { Thread } from "./types";
import type { FeedbackState } from "./state";

/** Is the shortcut modifier key pressed? Cmd on Mac, Ctrl elsewhere. */
export function modKey(e: KeyboardEvent): boolean {
  return IS_MAC ? e.metaKey : e.ctrlKey;
}

/** Generate a CSS selector for an element (for thread scoping). */
export function selectorFor(el: Element): string {
  if (el.id) return "#" + CSS.escape(el.id);
  const parts: string[] = [];
  let cur: Element | null = el;
  while (cur && cur !== document.body && cur !== document.documentElement) {
    let seg = cur.tagName.toLowerCase();
    if (cur.id) {
      parts.unshift("#" + CSS.escape(cur.id));
      break;
    }
    if (cur.className && typeof cur.className === "string") {
      const classes = cur.className
        .trim()
        .split(/\s+/)
        .filter((c) => c && !c.startsWith(PREFIX));
      if (classes.length) {
        seg += "." + classes.map(CSS.escape).join(".");
      }
    }
    const parent: Element | null = cur.parentElement;
    if (parent) {
      const siblings = Array.from(parent.children).filter(
        (s: Element) => s.tagName === cur!.tagName,
      );
      if (siblings.length > 1) {
        seg +=
          ":nth-child(" +
          (Array.from(parent.children).indexOf(cur) + 1) +
          ")";
      }
    }
    parts.unshift(seg);
    cur = parent;
  }
  return parts.join(" > ");
}

/** Create a DOM element with veld-feedback- prefixed class names. */
export function mkEl(tag: string, cls?: string, text?: string): HTMLElement {
  const el = document.createElement(tag);
  if (cls)
    el.className = cls
      .split(" ")
      .map((c) => PREFIX + c)
      .join(" ");
  if (text !== undefined) el.textContent = text;
  return el;
}

/** Get element position relative to document (not viewport). */
export function docRect(el: Element): {
  x: number;
  y: number;
  width: number;
  height: number;
} {
  const r = el.getBoundingClientRect();
  return {
    x: r.left + window.scrollX,
    y: r.top + window.scrollY,
    width: r.width,
    height: r.height,
  };
}

/** Wire Cmd+Enter / Ctrl+Enter on a textarea to click a button. */
export function submitOnModEnter(
  textarea: HTMLElement,
  btn: HTMLElement,
): void {
  textarea.addEventListener("keydown", function (e) {
    if ((e as KeyboardEvent).key === "Enter" && modKey(e as KeyboardEvent)) {
      e.preventDefault();
      (btn as HTMLButtonElement).click();
    }
  });
}

/** Relative time string: "2m ago", "3h ago", "1d ago". */
export function timeAgo(dateStr: string): string {
  const now = Date.now();
  const then = new Date(dateStr).getTime();
  const seconds = Math.floor((now - then) / 1000);
  if (seconds < 60) return seconds + "s ago";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return minutes + "m ago";
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return hours + "h ago";
  const days = Math.floor(hours / 24);
  return days + "d ago";
}

/** Check if a thread has messages the user hasn't seen. */
export function hasUnread(thread: Thread, lastSeenAt: Record<string, number>): boolean {
  if (!thread.messages.length) return false;
  const lastSeen = lastSeenAt[thread.id] || 0;
  for (let i = 0; i < thread.messages.length; i++) {
    if (
      thread.messages[i].author === "agent" &&
      new Date(thread.messages[i].created_at).getTime() > lastSeen
    ) {
      return true;
    }
  }
  return false;
}

/** Check if a URL matches the current page. */
export function isCurrentPage(url: string): boolean {
  return window.location.pathname === url;
}

/** Get the page URL from a thread's scope. */
export function getThreadPageUrl(thread: Thread): string {
  return thread.scope.page_url || "/";
}

/** Get thread element position (for pins). */
export function getThreadPosition(
  thread: Thread,
): { x: number; y: number; width: number; height: number } | null {
  return thread.scope.position || null;
}

/** Find a thread by ID. */
export function findThread(
  threads: Thread[],
  id: string,
): Thread | undefined {
  return threads.find((t) => t.id === id);
}

/** Deduplicate and truncate a component trace. */
export function formatTrace(trace: string[] | null): string | null {
  if (!trace || !trace.length) return null;
  let deduped = [trace[0]];
  for (let i = 1; i < trace.length; i++) {
    if (trace[i] !== trace[i - 1]) deduped.push(trace[i]);
  }
  if (deduped.length > 5) deduped = deduped.slice(deduped.length - 5);
  return deduped.join(" > ");
}
