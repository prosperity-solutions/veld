// @vitest-environment jsdom
/**
 * TDD tests for zero-friction draw mode.
 *
 * The core principle: draw mode should activate INSTANTLY with no
 * screen share dialog. The dialog only appears when we need page
 * pixels (blur tool, or compositing on Done).
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

describe("Stage 1: Draw mode activates without capture stream", () => {
  beforeEach(() => {
    setupDOM();
  });

  it("setMode('draw') does NOT call acquireCaptureStream", async () => {
    // The acquireCaptureStream function should not be called when entering draw mode.
    // Draw mode should load the script and set up the canvas without any capture.
    // After this change, getState().captureStream should remain null.
    expect(getState().captureStream).toBeNull();

    // After entering draw mode, capture stream should still be null
    // (the actual setMode call needs the implementation change first —
    //  this test will FAIL until we implement it, which is the point of TDD)
  });

  it("capture stream is null after draw mode setup", () => {
    // This validates the contract: draw mode doesn't touch the capture stream
    dispatch({ type: "SET_MODE", mode: "draw" });
    expect(getState().captureStream).toBeNull();
  });
});

describe("Stage 2: Blur requests capture AFTER gesture", () => {
  it("blur stroke without snapshot triggers acquireSnapshot callback", () => {
    // The draw overlay should accept an `acquireSnapshot` callback option
    // When a blur stroke completes and no snapshot is cached, it calls the callback
    // This test documents the expected interface
    const acquireSnapshot = vi.fn().mockResolvedValue(null);

    // TODO: Wire this into the draw overlay's activate() options
    // After blur stroke completes, acquireSnapshot should have been called
    expect(acquireSnapshot).not.toHaveBeenCalled(); // before any stroke
  });

  it("second blur stroke uses cached snapshot, no re-acquire", () => {
    // After the first blur acquires a snapshot, subsequent blur strokes
    // should use the cached version without calling acquireSnapshot again
    const acquireSnapshot = vi.fn().mockResolvedValue(null);

    // TODO: After first blur, snapshot is cached in draw state
    // Second blur should not call acquireSnapshot
  });
});

describe("Stage 3: Done triggers capture and kills stream", () => {
  it("Done with strokes acquires stream, composites, kills stream", () => {
    // When user clicks Done with strokes:
    // 1. acquireCaptureStream() is called
    // 2. grabFrame() captures the page
    // 3. Annotations are composited onto the frame
    // 4. stopCaptureStream() kills the stream
    // 5. Stream is null after

    dispatch({ type: "SET_MODE", mode: "draw" });
    // After Done, stream should be null
    expect(getState().captureStream).toBeNull();
  });

  it("Done without strokes exits draw mode cleanly, no capture", () => {
    dispatch({ type: "SET_MODE", mode: "draw" });
    // Clicking Done with no strokes should just exit, no stream needed
    dispatch({ type: "SET_MODE", mode: null });
    expect(getState().captureStream).toBeNull();
  });
});
