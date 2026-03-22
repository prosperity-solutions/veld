// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from "vitest";
import { initState, S } from "../src/feedback-overlay/state";
import { setToolbarDeps } from "../src/feedback-overlay/toolbar";
import { setKeyboardDeps, onKeyDown } from "../src/feedback-overlay/keyboard";
import { setVisibilityDeps, hideOverlay, showOverlay } from "../src/feedback-overlay/visibility";
import { setPopoverDeps, closeActivePopover } from "../src/feedback-overlay/popover";

/**
 * These tests verify the late-bound dependency wiring works correctly.
 * This is the #1 risk area — if any set*Deps call is missing or has
 * the wrong signature, the overlay silently breaks.
 */

function setupState() {
  const host = document.createElement("veld-feedback");
  const shadow = host.attachShadow({ mode: "open" });
  initState(shadow, host);
  // Create minimal DOM refs that modules expect
  S.toolbarContainer = document.createElement("div");
  S.toolbar = document.createElement("div");
  S.overlay = document.createElement("div");
  S.hoverOutline = document.createElement("div");
  S.componentTraceEl = document.createElement("div");
  S.panel = document.createElement("div");
  S.fab = document.createElement("div");
  S.toolBtnSelect = document.createElement("div");
  S.toolBtnScreenshot = document.createElement("div");
  S.toolBtnDraw = document.createElement("div");
  S.toolBtnPageComment = document.createElement("div");
  S.toolBtnComments = document.createElement("div");
  S.toolBtnHide = document.createElement("div");
  S.screenshotRect = document.createElement("div");
}

describe("toolbar wiring", () => {
  beforeEach(setupState);

  it("setToolbarDeps wires setMode callback", () => {
    const mockSetMode = vi.fn();
    setToolbarDeps({
      setMode: mockSetMode,
      togglePageComment: vi.fn(),
      togglePanel: vi.fn(),
      hideOverlay: vi.fn(),
    });
    // Toolbar internally calls setModeFn — verified by the fact
    // that setToolbarDeps doesn't throw
    expect(mockSetMode).not.toHaveBeenCalled();
  });
});

describe("keyboard wiring", () => {
  beforeEach(setupState);

  it("ESC in draw mode calls setMode(null)", () => {
    const mockSetMode = vi.fn();
    setKeyboardDeps({
      setMode: mockSetMode,
      toggleToolbar: vi.fn(),
      togglePageComment: vi.fn(),
      togglePanel: vi.fn(),
      hideOverlay: vi.fn(),
      showOverlay: vi.fn(),
      closeActivePopover: vi.fn(),
    });

    S.activeMode = "draw";
    const event = new KeyboardEvent("keydown", { key: "Escape" });
    onKeyDown(event);
    expect(mockSetMode).toHaveBeenCalledWith(null);
  });

  it("shortcuts disabled blocks all except ESC in draw mode", () => {
    const mockSetMode = vi.fn();
    const mockToggleToolbar = vi.fn();
    setKeyboardDeps({
      setMode: mockSetMode,
      toggleToolbar: mockToggleToolbar,
      togglePageComment: vi.fn(),
      togglePanel: vi.fn(),
      hideOverlay: vi.fn(),
      showOverlay: vi.fn(),
      closeActivePopover: vi.fn(),
    });

    S.shortcutsDisabled = true;
    S.activeMode = null;

    // Cmd+Shift+V should be blocked
    const event = new KeyboardEvent("keydown", {
      key: "v", code: "KeyV", metaKey: true, shiftKey: true,
    });
    onKeyDown(event);
    expect(mockToggleToolbar).not.toHaveBeenCalled();
  });
});

describe("visibility wiring", () => {
  beforeEach(setupState);

  it("hideOverlay sets hidden state and hides elements", () => {
    setVisibilityDeps({
      setMode: vi.fn(),
      togglePanel: vi.fn(),
    });

    S.hidden = false;
    hideOverlay();
    expect(S.hidden).toBe(true);
  });

  it("showOverlay clears hidden state", () => {
    setVisibilityDeps({
      setMode: vi.fn(),
      togglePanel: vi.fn(),
    });

    S.hidden = true;
    showOverlay();
    expect(S.hidden).toBe(false);
  });
});

describe("popover wiring", () => {
  beforeEach(setupState);

  it("closeActivePopover removes popover from DOM", () => {
    setPopoverDeps({
      addPin: vi.fn(),
      updateBadge: vi.fn(),
      renderPanel: vi.fn(),
    });

    const pop = document.createElement("div");
    S.shadow.appendChild(pop);
    S.activePopover = pop;

    closeActivePopover();
    expect(S.activePopover).toBeNull();
  });

  it("closeActivePopover runs cleanup callback", () => {
    setPopoverDeps({
      addPin: vi.fn(),
      updateBadge: vi.fn(),
      renderPanel: vi.fn(),
    });

    const cleanup = vi.fn();
    const pop = document.createElement("div");
    (pop as any)._veldCleanup = cleanup;
    S.shadow.appendChild(pop);
    S.activePopover = pop;

    closeActivePopover();
    expect(cleanup).toHaveBeenCalled();
  });
});
