// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { toggleToolbar } from "../src/feedback-overlay/toolbar";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { PREFIX } from "../src/feedback-overlay/constants";
import { setupMockRefs } from "./test-helpers";

describe("toolbar overflow menu", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("closing toolbar collapses the overflow menu", () => {
    // Open toolbar
    dispatch({ type: "SET_TOOLBAR_OPEN", open: true });
    refs.toolbar.classList.add(PREFIX + "toolbar-open");

    // Simulate overflow being open
    refs.toolbarOverflow.classList.add(PREFIX + "toolbar-overflow-open");
    expect(refs.toolbarOverflow.classList.contains(PREFIX + "toolbar-overflow-open")).toBe(true);

    // Close toolbar via toggle
    toggleToolbar();

    expect(getState().toolbarOpen).toBe(false);
    expect(refs.toolbarOverflow.classList.contains(PREFIX + "toolbar-overflow-open")).toBe(false);
  });

  it("opening toolbar does not auto-expand overflow", () => {
    dispatch({ type: "SET_TOOLBAR_OPEN", open: false });
    toggleToolbar();

    expect(getState().toolbarOpen).toBe(true);
    expect(refs.toolbarOverflow.classList.contains(PREFIX + "toolbar-overflow-open")).toBe(false);
  });
});
