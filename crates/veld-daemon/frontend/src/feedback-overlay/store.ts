import type { Thread, UIMode, VeldPopoverElement } from "./types";

export type ThemeMode = "auto" | "dark" | "light";

export interface Store {
  // Data
  threads: Thread[];
  lastEventSeq: number;
  lastSeenAt: Record<string, number>;
  agentListening: boolean;

  // UI state
  panelOpen: boolean;
  panelTab: "active" | "resolved";
  activePopover: VeldPopoverElement | null;
  activeMode: UIMode;
  hoveredEl: Element | null;
  lockedEl: Element | null;
  toolbarOpen: boolean;
  hidden: boolean;
  shortcutsDisabled: boolean;
  theme: ThemeMode;
  expandedThreadId: string | null;
  pins: Record<string, HTMLElement>;

  // Draw/capture
  captureStream: MediaStream | null;
  drawLoaded: boolean;
  drawCleanup: (() => void) | null;
  drawCanvas: HTMLCanvasElement | null;
  prevOverflow: string | null;

  // FAB positioning
  fabCX: number;
  fabCY: number;
  fabWasDragged: boolean;

  // Misc
  rafPending: boolean;
  lastPathname: string;
}

export type Action =
  | { type: "SET_MODE"; mode: UIMode }
  | { type: "SET_TOOLBAR_OPEN"; open: boolean }
  | { type: "SET_PANEL_OPEN"; open: boolean }
  | { type: "SET_PANEL_TAB"; tab: "active" | "resolved" }
  | { type: "SET_EXPANDED_THREAD"; threadId: string | null }
  | { type: "SET_HIDDEN"; hidden: boolean }
  | { type: "SET_SHORTCUTS_DISABLED"; disabled: boolean }
  | { type: "SET_THEME"; theme: ThemeMode }
  | { type: "SET_THREADS"; threads: Thread[] }
  | { type: "ADD_THREAD"; thread: Thread }
  | { type: "SET_POPOVER"; popover: VeldPopoverElement | null }
  | { type: "SET_HOVERED"; el: Element | null }
  | { type: "SET_LOCKED"; el: Element | null }
  | { type: "SET_LISTENING"; listening: boolean }
  | { type: "MARK_SEEN"; threadId: string }
  | { type: "SET_CAPTURE_STREAM"; stream: MediaStream | null }
  | { type: "SET_DRAW_LOADED"; loaded: boolean }
  | { type: "SET_DRAW_CANVAS"; canvas: HTMLCanvasElement | null }
  | { type: "SET_DRAW_CLEANUP"; cleanup: (() => void) | null }
  | { type: "SET_PREV_OVERFLOW"; overflow: string | null }
  | { type: "SET_PIN"; threadId: string; el: HTMLElement }
  | { type: "REMOVE_PIN"; threadId: string }
  | { type: "CLEAR_PINS" }
  | { type: "SET_FAB_POS"; cx: number; cy: number }
  | { type: "SET_FAB_DRAGGED"; dragged: boolean }
  | { type: "SET_RAF_PENDING"; pending: boolean }
  | { type: "SET_LAST_EVENT_SEQ"; seq: number }
  | { type: "SET_LAST_PATHNAME"; path: string }
  ;

function reduce(s: Store, action: Action): Store {
  switch (action.type) {
    case "SET_MODE":
      return { ...s, activeMode: action.mode };
    case "SET_TOOLBAR_OPEN":
      return { ...s, toolbarOpen: action.open };
    case "SET_PANEL_OPEN":
      return { ...s, panelOpen: action.open };
    case "SET_PANEL_TAB":
      return { ...s, panelTab: action.tab };
    case "SET_EXPANDED_THREAD":
      return { ...s, expandedThreadId: action.threadId };
    case "SET_HIDDEN":
      return { ...s, hidden: action.hidden };
    case "SET_SHORTCUTS_DISABLED":
      return { ...s, shortcutsDisabled: action.disabled };
    case "SET_THEME":
      return { ...s, theme: action.theme };
    case "SET_THREADS":
      return { ...s, threads: action.threads };
    case "ADD_THREAD":
      return { ...s, threads: [...s.threads, action.thread] };
    case "SET_POPOVER":
      return { ...s, activePopover: action.popover };
    case "SET_HOVERED":
      return { ...s, hoveredEl: action.el };
    case "SET_LOCKED":
      return { ...s, lockedEl: action.el };
    case "SET_LISTENING":
      return { ...s, agentListening: action.listening };
    case "MARK_SEEN":
      return { ...s, lastSeenAt: { ...s.lastSeenAt, [action.threadId]: Date.now() } };
    case "SET_CAPTURE_STREAM":
      return { ...s, captureStream: action.stream };
    case "SET_DRAW_LOADED":
      return { ...s, drawLoaded: action.loaded };
    case "SET_DRAW_CANVAS":
      return { ...s, drawCanvas: action.canvas };
    case "SET_DRAW_CLEANUP":
      return { ...s, drawCleanup: action.cleanup };
    case "SET_PREV_OVERFLOW":
      return { ...s, prevOverflow: action.overflow };
    case "SET_PIN": {
      const pins = { ...s.pins, [action.threadId]: action.el };
      return { ...s, pins };
    }
    case "REMOVE_PIN": {
      const { [action.threadId]: _, ...rest } = s.pins;
      return { ...s, pins: rest };
    }
    case "CLEAR_PINS":
      return { ...s, pins: {} };
    case "SET_FAB_POS":
      return { ...s, fabCX: action.cx, fabCY: action.cy };
    case "SET_FAB_DRAGGED":
      return { ...s, fabWasDragged: action.dragged };
    case "SET_RAF_PENDING":
      return { ...s, rafPending: action.pending };
    case "SET_LAST_EVENT_SEQ":
      return { ...s, lastEventSeq: action.seq };
    case "SET_LAST_PATHNAME":
      return { ...s, lastPathname: action.path };
    default:
      return s;
  }
}

import { createStore, type Store as StoreInterface } from "../shared/create-store";

let instance: StoreInterface<Store, Action>;

function createInitial(): Store {
  return {
    threads: [],
    lastEventSeq: 0,
    lastSeenAt: {},
    agentListening: false,
    panelOpen: false,
    panelTab: "active",
    activePopover: null,
    activeMode: null,
    hoveredEl: null,
    lockedEl: null,
    toolbarOpen: false,
    hidden: false,
    shortcutsDisabled: false,
    theme: "auto",
    expandedThreadId: null,
    pins: {},
    captureStream: null,
    drawLoaded: false,
    drawCleanup: null,
    drawCanvas: null,
    prevOverflow: null,
    fabCX: 0,
    fabCY: 0,
    fabWasDragged: false,
    rafPending: false,
    lastPathname: typeof window !== "undefined" ? window.location.pathname : "/",
  };
}

export function initStore(): void {
  instance = createStore<Store, Action>(reduce, createInitial());
}

export function getState(): Readonly<Store> {
  return instance.getState();
}

export function dispatch(action: Action): void {
  instance.dispatch(action);
}
