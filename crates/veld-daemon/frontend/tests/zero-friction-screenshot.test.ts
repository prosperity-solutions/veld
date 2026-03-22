// @vitest-environment jsdom
/**
 * Regression tests for zero-friction screenshot mode.
 *
 * Two invariants:
 * 1. Entering screenshot mode does NOT acquire a capture stream — the browser
 *    permission dialog is deferred until after the user finishes drawing the
 *    selection rectangle.
 * 2. After the screenshot frame is grabbed, the capture stream is stopped
 *    immediately (no lingering stream).
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { registerDeps } from "../src/shared/registry";

function setupDOM() {
  const host = document.createElement("veld-feedback");
  const shadow = host.attachShadow({ mode: "open" });
  initState(shadow, host);

  // Create minimal DOM refs
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
}

describe("Screenshot mode: deferred stream acquisition", () => {
  beforeEach(() => {
    setupDOM();
  });

  it("setMode('screenshot') does NOT acquire a capture stream", async () => {
    // Wire up minimal deps so setMode works
    const { setMode } = await import("../src/feedback-overlay/modes");
    registerDeps({
      setMode,
      captureScreenshot: vi.fn(),
      showCreatePopover: vi.fn(),
      positionTooltip: vi.fn(),
      ensureDrawScript: vi.fn().mockResolvedValue(undefined),
    });

    // Capture stream must be null before entering screenshot mode
    expect(getState().captureStream).toBeNull();

    // Enter screenshot mode
    setMode("screenshot");

    // Capture stream must STILL be null — no getDisplayMedia called
    expect(getState().captureStream).toBeNull();

    // But the overlay and crosshair should be active (selection is ready)
    expect(refs.overlay.classList.contains("veld-feedback-overlay-active")).toBe(true);
    expect(refs.overlay.classList.contains("veld-feedback-overlay-crosshair")).toBe(true);
  });

  it("capture stream is null after exiting screenshot mode without selection", async () => {
    const { setMode } = await import("../src/feedback-overlay/modes");
    registerDeps({
      setMode,
      captureScreenshot: vi.fn(),
      showCreatePopover: vi.fn(),
      positionTooltip: vi.fn(),
      ensureDrawScript: vi.fn().mockResolvedValue(undefined),
    });

    setMode("screenshot");
    setMode(null);

    // No stream was ever acquired
    expect(getState().captureStream).toBeNull();
    // Overlay classes should be removed
    expect(refs.overlay.classList.contains("veld-feedback-overlay-active")).toBe(false);
    expect(refs.overlay.classList.contains("veld-feedback-overlay-crosshair")).toBe(false);
  });
});

describe("Screenshot capture: stream lifecycle", () => {
  beforeEach(() => {
    setupDOM();
  });

  it("captureScreenshot acquires stream then stops it after grab", async () => {
    const { setMode } = await import("../src/feedback-overlay/modes");
    const screenshot = await import("../src/feedback-overlay/screenshot");

    // Track whether stream tracks were stopped
    const stopFn = vi.fn();
    const mockTrack = {
      addEventListener: vi.fn(),
      stop: stopFn,
      kind: "video",
    };
    const mockStream = {
      getVideoTracks: () => [mockTrack],
      getTracks: () => [mockTrack],
    } as unknown as MediaStream;

    // Mock getDisplayMedia to return our tracked stream
    Object.defineProperty(navigator, "mediaDevices", {
      value: {
        getDisplayMedia: vi.fn().mockResolvedValue(mockStream),
      },
      configurable: true,
    });

    // Mock ImageCapture — grabFrame returns a minimal bitmap
    const mockBitmap = {
      width: 800,
      height: 600,
      close: vi.fn(),
    };
    (globalThis as any).ImageCapture = class {
      grabFrame() {
        return Promise.resolve(mockBitmap);
      }
    };

    registerDeps({
      setMode,
      captureScreenshot: screenshot.captureScreenshot,
      showCreatePopover: vi.fn(),
      positionTooltip: vi.fn(),
      ensureDrawScript: vi.fn().mockResolvedValue(undefined),
    });

    // Enter screenshot mode — no stream acquired
    setMode("screenshot");
    expect(getState().captureStream).toBeNull();

    // Simulate captureScreenshot (called after region selection)
    screenshot.captureScreenshot(10, 10, 200, 150);

    // Wait for the async acquireCaptureStream + grabFrame chain
    // acquireCaptureStream is a promise chain, so flush microtasks
    await vi.waitFor(() => {
      expect(stopFn).toHaveBeenCalled();
    }, { timeout: 2000 });

    // Stream should be null in state after capture completes
    expect(getState().captureStream).toBeNull();
  });
});
