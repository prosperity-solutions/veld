import { vi } from "vitest";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";
import { registerDeps } from "../src/shared/registry";
import type { Thread, Message } from "../src/feedback-overlay/types";

export function makeMessage(overrides: Partial<Message> = {}): Message {
  return {
    id: "m-" + Math.random().toString(36).slice(2, 8),
    body: "test message",
    author: "human" as const,
    created_at: new Date().toISOString(),
    ...overrides,
  };
}

export function makeThread(overrides: Partial<Thread> = {}): Thread {
  return {
    id: "t-" + Math.random().toString(36).slice(2, 8),
    scope: { type: "page", page_url: "/" },
    status: "open",
    messages: [],
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
    ...overrides,
  };
}

export function makeFakeDeps() {
  return {
    setMode: vi.fn(),
    toggleToolbar: vi.fn(),
    togglePanel: vi.fn(),
    togglePageComment: vi.fn(),
    hideOverlay: vi.fn(),
    showOverlay: vi.fn(),
    closeActivePopover: vi.fn(),
    addPin: vi.fn(),
    removePin: vi.fn(),
    renderAllPins: vi.fn(),
    renderPanel: vi.fn(),
    openThreadInPanel: vi.fn(),
    scrollToThread: vi.fn(),
    checkPendingScroll: vi.fn(),
    updateBadge: vi.fn(),
    captureScreenshot: vi.fn(),
    showCreatePopover: vi.fn(),
    positionTooltip: vi.fn(),
    ensureDrawScript: vi.fn().mockResolvedValue(undefined),
  };
}

export function setupOverlayEnv() {
  const host = document.createElement("veld-feedback");
  const shadow = host.attachShadow({ mode: "open" });
  document.body.appendChild(host);
  initState(shadow, host);

  const fakeDeps = makeFakeDeps();
  registerDeps(fakeDeps);

  return { shadow, host, deps: fakeDeps };
}

/** Create mock refs for modules that need DOM elements but don't use buildDOM(). */
export function setupMockRefs() {
  const env = setupOverlayEnv();

  // Set up panel refs
  refs.panel = document.createElement("div");
  refs.panelBody = document.createElement("div");
  refs.panelHeadTitle = document.createElement("span");
  refs.panelBackBtn = document.createElement("button");
  refs.markReadBtn = document.createElement("button");
  refs.segBtnActive = document.createElement("button");
  refs.segBtnResolved = document.createElement("button");

  // Set up toolbar refs
  refs.toolbar = document.createElement("div");
  refs.toolbarContainer = document.createElement("div");
  refs.fab = document.createElement("button");
  refs.fabBadge = document.createElement("span");
  refs.toolBtnSelect = document.createElement("button");
  refs.toolBtnScreenshot = document.createElement("button");
  refs.toolBtnDraw = document.createElement("button");
  refs.toolBtnPageComment = document.createElement("button");
  refs.toolBtnComments = document.createElement("button");
  refs.toolBtnHide = document.createElement("button");
  refs.listeningModule = document.createElement("div");

  // Set up light DOM refs
  refs.overlay = document.createElement("div");
  refs.hoverOutline = document.createElement("div");
  refs.componentTraceEl = document.createElement("div");
  refs.screenshotRect = document.createElement("div");

  // Set up tooltip
  refs.tooltip = document.createElement("div");

  // Wire panel structure so parent queries work
  const panelHead = document.createElement("div");
  const segmented = document.createElement("div");
  segmented.className = "veld-feedback-segmented";
  panelHead.appendChild(refs.panelBackBtn);
  panelHead.appendChild(segmented);
  panelHead.appendChild(refs.markReadBtn);
  refs.panel.appendChild(panelHead);
  refs.panel.appendChild(refs.panelBody);
  env.shadow.appendChild(refs.panel);

  return env;
}
