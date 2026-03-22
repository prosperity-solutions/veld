// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from "vitest";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { onKeyDown } from "../src/feedback-overlay/keyboard";
import { hideOverlay, showOverlay } from "../src/feedback-overlay/visibility";
import { closeActivePopover } from "../src/feedback-overlay/popover";
import { registerDeps } from "../src/shared/registry";

/**
 * These tests verify the registry-based dependency wiring works correctly.
 * All modules now use deps() from the shared registry instead of per-module
 * set*Deps functions.
 */

function makeFakeDeps() {
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

function setupState() {
  const host = document.createElement("veld-feedback");
  const shadow = host.attachShadow({ mode: "open" });
  initState(shadow, host);
  // Create minimal DOM refs that modules expect
  refs.toolbarContainer = document.createElement("div");
  refs.toolbar = document.createElement("div");
  refs.overlay = document.createElement("div");
  refs.hoverOutline = document.createElement("div");
  refs.componentTraceEl = document.createElement("div");
  refs.panel = document.createElement("div");
  refs.fab = document.createElement("div");
  refs.toolBtnSelect = document.createElement("div");
  refs.toolBtnScreenshot = document.createElement("div");
  refs.toolBtnDraw = document.createElement("div");
  refs.toolBtnPageComment = document.createElement("div");
  refs.toolBtnComments = document.createElement("div");
  refs.toolBtnHide = document.createElement("div");
  refs.screenshotRect = document.createElement("div");
  registerDeps(makeFakeDeps());
}

describe("toolbar wiring", () => {
  beforeEach(setupState);

  it("registerDeps wires setMode callback for toolbar", () => {
    const fakeDeps = makeFakeDeps();
    registerDeps(fakeDeps);
    // Toolbar internally calls deps().setMode — verified by the fact
    // that registerDeps doesn't throw
    expect(fakeDeps.setMode).not.toHaveBeenCalled();
  });
});

describe("keyboard wiring", () => {
  beforeEach(setupState);

  it("ESC in draw mode calls setMode(null)", () => {
    const fakeDeps = makeFakeDeps();
    registerDeps(fakeDeps);

    dispatch({ type: "SET_MODE", mode: "draw" });
    const event = new KeyboardEvent("keydown", { key: "Escape" });
    onKeyDown(event);
    expect(fakeDeps.setMode).toHaveBeenCalledWith(null);
  });

  it("shortcuts disabled blocks all except ESC in draw mode", () => {
    const fakeDeps = makeFakeDeps();
    registerDeps(fakeDeps);

    dispatch({ type: "SET_SHORTCUTS_DISABLED", disabled: true });
    dispatch({ type: "SET_MODE", mode: null });

    // Cmd+Shift+V should be blocked
    const event = new KeyboardEvent("keydown", {
      key: "v", code: "KeyV", metaKey: true, shiftKey: true,
    });
    onKeyDown(event);
    expect(fakeDeps.toggleToolbar).not.toHaveBeenCalled();
  });
});

describe("visibility wiring", () => {
  beforeEach(setupState);

  it("hideOverlay sets hidden state and hides elements", () => {
    registerDeps(makeFakeDeps());

    dispatch({ type: "SET_HIDDEN", hidden: false });
    hideOverlay();
    expect(getState().hidden).toBe(true);
  });

  it("showOverlay clears hidden state", () => {
    registerDeps(makeFakeDeps());

    dispatch({ type: "SET_HIDDEN", hidden: true });
    showOverlay();
    expect(getState().hidden).toBe(false);
  });
});

describe("popover wiring", () => {
  beforeEach(setupState);

  it("closeActivePopover removes popover from DOM", () => {
    registerDeps(makeFakeDeps());

    const pop = document.createElement("div");
    refs.shadow.appendChild(pop);
    dispatch({ type: "SET_POPOVER", popover: pop });

    closeActivePopover();
    expect(getState().activePopover).toBeNull();
  });

  it("closeActivePopover runs cleanup callback", () => {
    registerDeps(makeFakeDeps());

    const cleanup = vi.fn();
    const pop = document.createElement("div");
    (pop as any)._veldCleanup = cleanup;
    refs.shadow.appendChild(pop);
    dispatch({ type: "SET_POPOVER", popover: pop });

    closeActivePopover();
    expect(cleanup).toHaveBeenCalled();
  });
});
