import type { Thread, UIMode } from "./types";

export type ThemeMode = "auto" | "dark" | "light";

export interface FeedbackState {
  // Data
  threads: Thread[];
  lastEventSeq: number;
  lastSeenAt: Record<string, number>;
  agentListening: boolean;

  // UI state
  panelOpen: boolean;
  panelTab: "active" | "resolved";
  activePopover: HTMLElement | null;
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

  // DOM roots
  shadow: ShadowRoot;
  hostEl: HTMLElement;

  // DOM refs — toolbar
  toolbarContainer: HTMLElement;
  fab: HTMLElement;
  fabBadge: HTMLElement;
  toolbar: HTMLElement;
  toolBtnSelect: HTMLElement;
  toolBtnScreenshot: HTMLElement;
  toolBtnDraw: HTMLElement;
  toolBtnPageComment: HTMLElement;
  toolBtnComments: HTMLElement;
  toolBtnHide: HTMLElement;
  listeningModule: HTMLElement;

  // DOM refs — light DOM
  overlay: HTMLElement;
  hoverOutline: HTMLElement;
  componentTraceEl: HTMLElement;
  screenshotRect: HTMLElement;

  // DOM refs — panel
  panel: HTMLElement;
  panelBody: HTMLElement;
  panelHeadTitle: HTMLElement;
  panelBackBtn: HTMLElement;
  markReadBtn: HTMLElement;
  segBtnActive: HTMLElement;
  segBtnResolved: HTMLElement;

  // DOM refs — tooltip
  tooltip: HTMLElement;

  // FAB positioning
  fabCX: number;
  fabCY: number;

  // Reposition throttle
  rafPending: boolean;

  // SPA navigation
  lastPathname: string;
}

/** Mutable singleton — initialized by initState(), then imported by all modules. */
// eslint-disable-next-line prefer-const
export let S: FeedbackState;

export function initState(shadow: ShadowRoot, hostEl: HTMLElement): void {
  S = {
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
    shadow,
    hostEl,
    // DOM refs — set during buildDOM
    toolbarContainer: null!,
    fab: null!,
    fabBadge: null!,
    toolbar: null!,
    toolBtnSelect: null!,
    toolBtnScreenshot: null!,
    toolBtnDraw: null!,
    toolBtnPageComment: null!,
    toolBtnComments: null!,
    toolBtnHide: null!,
    listeningModule: null!,
    overlay: null!,
    hoverOutline: null!,
    componentTraceEl: null!,
    screenshotRect: null!,
    panel: null!,
    panelBody: null!,
    panelHeadTitle: null!,
    panelBackBtn: null!,
    markReadBtn: null!,
    segBtnActive: null!,
    segBtnResolved: null!,
    tooltip: null!,
    fabCX: 0,
    fabCY: 0,
    rafPending: false,
    lastPathname: window.location.pathname,
  };
}
