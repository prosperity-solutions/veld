/**
 * Backward-compatible state shim.
 *
 * `S` is a Proxy that routes reads/writes to either `refs` (DOM references)
 * or `store` (reactive data state managed by a reducer).
 *
 * Reads go directly to refs/store. Writes to store fields go through
 * dispatch() so all mutations are centralized.
 *
 * Modules can import `S` and use it exactly as before:
 *   S.threads      → reads store.threads
 *   S.shadow       → reads refs.shadow
 *   S.panelOpen = true  → dispatch({ type: "SET_PANEL_OPEN", open: true })
 *
 * New code should import { refs } from "./refs" and { store, dispatch } from "./store" directly.
 */

import { refs, initRefs } from "./refs";
import type { DOMRefs } from "./refs";
import { store, dispatch, initStore } from "./store";
import type { Store, ThemeMode, Action } from "./store";
import type { FeedbackEvent, Thread, UIMode, VeldPopoverElement } from "./types";

// Re-export for backward compat
export type { ThemeMode } from "./store";
export { refs } from "./refs";
export { store, dispatch } from "./store";

// The full S type is the union of DOMRefs + Store
export type FeedbackState = DOMRefs & Store;

// Map store field names to dispatch actions for setter proxy
const STORE_KEYS = new Set<string>([
  "threads", "lastEventSeq", "lastSeenAt", "agentListening",
  "panelOpen", "panelTab", "activePopover", "activeMode",
  "hoveredEl", "lockedEl", "toolbarOpen", "hidden",
  "shortcutsDisabled", "theme", "expandedThreadId", "pins",
  "captureStream", "drawLoaded", "drawCleanup", "drawCanvas",
  "prevOverflow", "fabCX", "fabCY", "fabWasDragged",
  "rafPending", "lastPathname",
]);

// Map field name → action creator for simple setters
function makeAction(key: string, value: unknown): Action | null {
  switch (key) {
    case "activeMode": return { type: "SET_MODE", mode: value as UIMode };
    case "toolbarOpen": return { type: "SET_TOOLBAR_OPEN", open: value as boolean };
    case "panelOpen": return { type: "SET_PANEL_OPEN", open: value as boolean };
    case "panelTab": return { type: "SET_PANEL_TAB", tab: value as "active" | "resolved" };
    case "expandedThreadId": return { type: "SET_EXPANDED_THREAD", threadId: value as string | null };
    case "hidden": return { type: "SET_HIDDEN", hidden: value as boolean };
    case "shortcutsDisabled": return { type: "SET_SHORTCUTS_DISABLED", disabled: value as boolean };
    case "theme": return { type: "SET_THEME", theme: value as ThemeMode };
    case "threads": return { type: "SET_THREADS", threads: value as Thread[] };
    case "activePopover": return { type: "SET_POPOVER", popover: value as VeldPopoverElement | null };
    case "hoveredEl": return { type: "SET_HOVERED", el: value as Element | null };
    case "lockedEl": return { type: "SET_LOCKED", el: value as Element | null };
    case "agentListening": return { type: "SET_LISTENING", listening: value as boolean };
    case "captureStream": return { type: "SET_CAPTURE_STREAM", stream: value as MediaStream | null };
    case "drawLoaded": return { type: "SET_DRAW_LOADED", loaded: value as boolean };
    case "drawCanvas": return { type: "SET_DRAW_CANVAS", canvas: value as HTMLCanvasElement | null };
    case "drawCleanup": return { type: "SET_DRAW_CLEANUP", cleanup: value as (() => void) | null };
    case "prevOverflow": return { type: "SET_PREV_OVERFLOW", overflow: value as string | null };
    case "fabWasDragged": return { type: "SET_FAB_DRAGGED", dragged: value as boolean };
    case "rafPending": return { type: "SET_RAF_PENDING", pending: value as boolean };
    case "lastPathname": return { type: "SET_LAST_PATHNAME", path: value as string };
    // These are objects that get mutated in-place (pins, lastSeenAt, lastEventSeq)
    // — handle via direct store mutation for backward compat
    case "lastEventSeq": return { type: "SET_LAST_EVENT_SEQ", seq: value as number };
    default: return null;
  }
}

/** Backward-compatible state proxy. Reads from refs/store, writes dispatch actions. */
export const S: FeedbackState = new Proxy({} as FeedbackState, {
  get(_target, prop: string) {
    if (STORE_KEYS.has(prop)) return (store as unknown as Record<string, unknown>)[prop];
    return (refs as unknown as Record<string, unknown>)[prop];
  },
  set(_target, prop: string, value: unknown) {
    if (STORE_KEYS.has(prop)) {
      const action = makeAction(prop, value);
      if (action) {
        dispatch(action);
      } else {
        // Fallback: direct mutation for complex fields (pins, lastSeenAt)
        (store as unknown as Record<string, unknown>)[prop] = value;
      }
    } else {
      // DOM ref assignment (during buildDOM)
      (refs as unknown as Record<string, unknown>)[prop] = value;
    }
    return true;
  },
});

export function initState(shadow: ShadowRoot, hostEl: HTMLElement): void {
  initRefs(shadow, hostEl);
  initStore();
}
