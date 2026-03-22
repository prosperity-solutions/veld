/**
 * State module — re-exports refs, store, and dispatch.
 *
 * The Proxy-based `S` shim has been removed. All modules now import
 * `refs`, `store`, and `dispatch` directly from "./refs" and "./store".
 *
 * This module exists for backward compatibility (e.g. tests that import
 * `initState`) and as a convenience re-export hub.
 */

import { initRefs } from "./refs";
import { initStore } from "./store";

export type { DOMRefs } from "./refs";
export type { ThemeMode, Store } from "./store";
export { refs } from "./refs";
export { store, dispatch } from "./store";

// The full FeedbackState type is the union of DOMRefs + Store
import type { DOMRefs } from "./refs";
import type { Store } from "./store";
export type FeedbackState = DOMRefs & Store;

export function initState(shadow: ShadowRoot, hostEl: HTMLElement): void {
  initRefs(shadow, hostEl);
  initStore();
}
