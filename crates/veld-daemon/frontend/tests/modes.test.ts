// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { setMode } from "../src/feedback-overlay/modes";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { PREFIX } from "../src/feedback-overlay/constants";
import { setupMockRefs } from "./test-helpers";

describe("setMode", () => {
  beforeEach(() => {
    setupMockRefs();
  });

  it("select-element adds overlay-active class", () => {
    setMode("select-element");
    expect(refs.overlay.classList.contains(PREFIX + "overlay-active")).toBe(true);
    expect(getState().activeMode).toBe("select-element");
  });

  it("select-element sets tool-active on select button", () => {
    setMode("select-element");
    expect(refs.toolBtnSelect.classList.contains(PREFIX + "tool-active")).toBe(true);
    expect(refs.toolBtnScreenshot.classList.contains(PREFIX + "tool-active")).toBe(false);
  });

  it("null from select-element tears down correctly", () => {
    setMode("select-element");
    setMode(null);

    expect(refs.overlay.classList.contains(PREFIX + "overlay-active")).toBe(false);
    expect(refs.hoverOutline.style.display).toBe("none");
    expect(refs.componentTraceEl.style.display).toBe("none");
    expect(getState().hoveredEl).toBeNull();
    expect(getState().lockedEl).toBeNull();
    expect(getState().activeMode).toBeNull();
  });

  it("null removes tool-active from all buttons", () => {
    setMode("select-element");
    setMode(null);

    expect(refs.toolBtnSelect.classList.contains(PREFIX + "tool-active")).toBe(false);
    expect(refs.toolBtnScreenshot.classList.contains(PREFIX + "tool-active")).toBe(false);
    expect(refs.toolBtnDraw.classList.contains(PREFIX + "tool-active")).toBe(false);
  });

  it("screenshot adds overlay-active and overlay-crosshair", () => {
    setMode("screenshot");
    expect(refs.overlay.classList.contains(PREFIX + "overlay-active")).toBe(true);
    expect(refs.overlay.classList.contains(PREFIX + "overlay-crosshair")).toBe(true);
    expect(refs.toolBtnScreenshot.classList.contains(PREFIX + "tool-active")).toBe(true);
  });

  it("null from screenshot removes crosshair and hides rect", () => {
    setMode("screenshot");
    setMode(null);

    expect(refs.overlay.classList.contains(PREFIX + "overlay-active")).toBe(false);
    expect(refs.overlay.classList.contains(PREFIX + "overlay-crosshair")).toBe(false);
    expect(refs.screenshotRect.style.display).toBe("none");
  });

  it("switching modes tears down previous mode first", () => {
    setMode("select-element");
    setMode("screenshot");

    // select-element teardown should have happened
    expect(refs.hoverOutline.style.display).toBe("none");
    // screenshot should be active
    expect(refs.overlay.classList.contains(PREFIX + "overlay-crosshair")).toBe(true);
    expect(getState().activeMode).toBe("screenshot");
  });

  it("setMode(null) when no mode active is a no-op", () => {
    setupMockRefs();
    // Should not throw or change anything
    setMode(null);
    expect(getState().activeMode).toBeNull();
    expect(refs.overlay.classList.contains(PREFIX + "overlay-active")).toBe(false);
  });

  it("setMode(null) from draw mode tears down draw", () => {
    // Simulate being in draw mode with a canvas in the DOM
    dispatch({ type: "SET_MODE", mode: "draw" });
    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);
    dispatch({ type: "SET_DRAW_CANVAS", canvas });
    dispatch({ type: "SET_DRAW_CLEANUP", cleanup: () => {} });

    setMode(null);

    expect(getState().activeMode).toBeNull();
    expect(getState().drawCanvas).toBeNull();
    expect(canvas.parentNode).toBeNull();
  });

  it("setMode('select-element') from draw mode tears down draw first", () => {
    dispatch({ type: "SET_MODE", mode: "draw" });
    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);
    dispatch({ type: "SET_DRAW_CANVAS", canvas });
    dispatch({ type: "SET_DRAW_CLEANUP", cleanup: () => {} });

    setMode("select-element");

    // draw teardown should have happened
    expect(canvas.parentNode).toBeNull();
    expect(getState().drawCanvas).toBeNull();
    // select-element should be active
    expect(getState().activeMode).toBe("select-element");
    expect(refs.overlay.classList.contains(PREFIX + "overlay-active")).toBe(true);
  });

  it("setMode(null) from draw mode succeeds even if teardown throws", () => {
    dispatch({ type: "SET_MODE", mode: "draw" });
    const canvas = document.createElement("canvas");
    document.body.appendChild(canvas);
    dispatch({ type: "SET_DRAW_CANVAS", canvas });
    dispatch({
      type: "SET_DRAW_CLEANUP",
      cleanup: () => { throw new Error("cleanup throws"); },
    });

    // Should not throw
    setMode(null);

    // Mode should still transition
    expect(getState().activeMode).toBeNull();
    // Canvas should be force-removed by the catch block
    expect(getState().drawCanvas).toBeNull();
  });
});
