// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { positionPopover, closeActivePopover } from "../src/feedback-overlay/popover";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";
import { getState, dispatch } from "../src/feedback-overlay/store";
import { setPopoverDeps } from "../src/feedback-overlay/popover";
import { vi } from "vitest";

function mockEl(): HTMLElement {
  return document.createElement("div");
}

describe("positionPopover", () => {
  it("positions below anchor with gap", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 200, y: 100, width: 150, height: 40 });
    const top = parseFloat(pop.style.top!);
    // Should be below: y + height + gap (10)
    expect(top).toBeGreaterThanOrEqual(150); // 100 + 40 + 10
  });

  it("centers horizontally on anchor", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 400, y: 100, width: 200, height: 40 });
    const left = parseFloat(pop.style.left!);
    // Centered: x + width/2 - popWidth/2 = 400 + 100 - 180 = 320
    expect(left).toBeGreaterThan(200);
    expect(left).toBeLessThan(500);
  });

  it("clamps left position to minimum margin", () => {
    const pop = mockEl();
    // Anchor at far left — popover should not go negative
    positionPopover(pop, { x: 0, y: 100, width: 10, height: 10 });
    const left = parseFloat(pop.style.left!);
    expect(left).toBeGreaterThanOrEqual(0);
  });

  it("outputs pixel values", () => {
    const pop = mockEl();
    positionPopover(pop, { x: 500, y: 200, width: 100, height: 50 });
    expect(pop.style.top).toMatch(/^\d+(\.\d+)?px$/);
    expect(pop.style.left).toMatch(/^\d+(\.\d+)?px$/);
  });
});

describe("closeActivePopover", () => {
  beforeEach(() => {
    const host = document.createElement("veld-feedback");
    const shadow = host.attachShadow({ mode: "open" });
    initState(shadow, host);
    refs.hoverOutline = document.createElement("div");
    refs.componentTraceEl = document.createElement("div");
    refs.toolBtnPageComment = document.createElement("div");
    refs.toolBtnScreenshot = document.createElement("div");
    setPopoverDeps({ addPin: vi.fn(), updateBadge: vi.fn(), renderPanel: vi.fn() });
  });

  it("removes popover element and nulls reference", () => {
    const pop = document.createElement("div");
    refs.shadow.appendChild(pop);
    dispatch({ type: "SET_POPOVER", popover: pop });

    closeActivePopover();

    expect(getState().activePopover).toBeNull();
    expect(pop.parentNode).toBeNull();
  });

  it("calls _veldCleanup if present", () => {
    const cleanup = vi.fn();
    const pop = document.createElement("div");
    (pop as any)._veldCleanup = cleanup;
    refs.shadow.appendChild(pop);
    dispatch({ type: "SET_POPOVER", popover: pop });

    closeActivePopover();

    expect(cleanup).toHaveBeenCalledOnce();
  });

  it("clears locked element and hides outlines", () => {
    const pop = document.createElement("div");
    refs.shadow.appendChild(pop);
    dispatch({ type: "SET_POPOVER", popover: pop });
    dispatch({ type: "SET_LOCKED", el: document.createElement("div") });
    refs.hoverOutline.style.display = "block";

    closeActivePopover();

    expect(getState().lockedEl).toBeNull();
    expect(refs.hoverOutline.style.display).toBe("none");
  });

  it("does nothing when no active popover", () => {
    dispatch({ type: "SET_POPOVER", popover: null });
    dispatch({ type: "SET_LOCKED", el: null });
    // Should not throw
    closeActivePopover();
    expect(getState().activePopover).toBeNull();
  });
});
