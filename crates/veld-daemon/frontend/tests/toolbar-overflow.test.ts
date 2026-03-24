// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { toggleToolbar } from "../src/feedback-overlay/toolbar";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { setupMockRefs } from "./test-helpers";

describe("toolbar overflow menu", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("closing toolbar resets overflow state", () => {
    // Open toolbar
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    // Simulate overflow being open
    dispatch({ type: "SET_OVERFLOW_OPEN", open: true });
    expect(getState().overflowOpen).toBe(true);

    // Close toolbar via toggle
    toggleToolbar();

    expect(getState().toolbarOpen).toBe(false);
    expect(getState().overflowOpen).toBe(false);
  });

  it("opening toolbar does not auto-expand overflow", () => {
    dispatch({ type: "SET_TOOLBAR_OPEN", open: false });
    toggleToolbar();

    expect(getState().toolbarOpen).toBe(true);
    expect(getState().overflowOpen).toBe(false);
  });
});
