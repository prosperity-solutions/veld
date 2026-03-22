// @vitest-environment jsdom
import { describe, it, expect, vi } from "vitest";
import { createXYPad } from "../src/feedback-overlay/xy-pad";
import { createControlsRegistry } from "../src/shared/controls";
import type { AxisDef } from "../src/shared/controls";

function makeAxis(overrides: Partial<AxisDef> = {}): AxisDef {
  return { name: "x", value: 50, min: 0, max: 100, step: 1, label: "X", ...overrides };
}

function mockPadRect(pad: HTMLElement, width = 200, height = 200): void {
  Object.defineProperty(pad, "getBoundingClientRect", {
    value: () => ({ left: 0, top: 0, width, height, right: width, bottom: height, x: 0, y: 0, toJSON() {} }),
  });
}

describe("createXYPad", () => {
  it("returns element and cleanup", () => {
    const reg = createControlsRegistry();
    const result = createXYPad(makeAxis({ name: "a" }), makeAxis({ name: "b" }), reg, vi.fn());
    expect(result.element).toBeInstanceOf(HTMLElement);
    expect(typeof result.cleanup).toBe("function");
  });

  it("sets initial values in registry", () => {
    const reg = createControlsRegistry();
    createXYPad(
      makeAxis({ name: "dur", value: 200 }),
      makeAxis({ name: "ease", value: 0.5 }),
      reg,
      vi.fn(),
    );
    expect(reg.get("dur")).toBe(200);
    expect(reg.get("ease")).toBe(0.5);
  });

  it("shows header with both axis names", () => {
    const reg = createControlsRegistry();
    const { element } = createXYPad(
      makeAxis({ name: "a", label: "Duration" }),
      makeAxis({ name: "b", label: "Easing" }),
      reg,
      vi.fn(),
    );
    expect(element.textContent).toContain("Duration");
    expect(element.textContent).toContain("Easing");
  });

  it("has a split button that calls onSplit", () => {
    const reg = createControlsRegistry();
    const onSplit = vi.fn();
    const { element } = createXYPad(makeAxis({ name: "a" }), makeAxis({ name: "b" }), reg, onSplit);
    const btn = element.querySelector(".veld-feedback-xy-split-btn") as HTMLButtonElement;
    expect(btn).not.toBeNull();
    btn.click();
    expect(onSplit).toHaveBeenCalledOnce();
  });

  it("contains a pad with dot", () => {
    const reg = createControlsRegistry();
    const { element } = createXYPad(makeAxis({ name: "a" }), makeAxis({ name: "b" }), reg, vi.fn());
    expect(element.querySelector(".veld-feedback-xy-pad")).not.toBeNull();
    expect(element.querySelector(".veld-feedback-xy-dot")).not.toBeNull();
  });

  it("updates registry on pointer down+up at center", () => {
    const reg = createControlsRegistry();
    const xAxis = makeAxis({ name: "dur", value: 0, min: 0, max: 200, step: 1 });
    const yAxis = makeAxis({ name: "amp", value: 0, min: 0, max: 100, step: 1 });
    const { element } = createXYPad(xAxis, yAxis, reg, vi.fn());
    const pad = element.querySelector(".veld-feedback-xy-pad") as HTMLElement;
    mockPadRect(pad);

    pad.dispatchEvent(new PointerEvent("pointerdown", { clientX: 100, clientY: 100, bubbles: true }));
    pad.dispatchEvent(new PointerEvent("pointerup", { clientX: 100, clientY: 100, bubbles: true }));

    expect(reg.get("dur")).toBe(100);  // midpoint of 0-200
    expect(reg.get("amp")).toBe(50);   // midpoint of 0-100 (y inverted)
  });

  it("clamps to axis bounds", () => {
    const reg = createControlsRegistry();
    const xAxis = makeAxis({ name: "a", min: 10, max: 20, step: 1 });
    const yAxis = makeAxis({ name: "b", min: -5, max: 5, step: 1 });
    const { element } = createXYPad(xAxis, yAxis, reg, vi.fn());
    const pad = element.querySelector(".veld-feedback-xy-pad") as HTMLElement;
    mockPadRect(pad);

    // Far outside top-left
    pad.dispatchEvent(new PointerEvent("pointerdown", { clientX: -999, clientY: -999, bubbles: true }));
    pad.dispatchEvent(new PointerEvent("pointerup", { clientX: -999, clientY: -999, bubbles: true }));

    expect(reg.get("a")).toBe(10);  // min
    expect(reg.get("b")).toBe(5);   // max (top = max y)
  });

  it("snaps to step", () => {
    const reg = createControlsRegistry();
    const xAxis = makeAxis({ name: "a", min: 0, max: 100, step: 25 });
    const yAxis = makeAxis({ name: "b", min: 0, max: 1, step: 0.5 });
    const { element } = createXYPad(xAxis, yAxis, reg, vi.fn());
    const pad = element.querySelector(".veld-feedback-xy-pad") as HTMLElement;
    mockPadRect(pad);

    // x: 60/200=0.3 → 30 → snap to 25
    // y: (1 - 80/200)=0.6 → 0.6 → snap to 0.5
    pad.dispatchEvent(new PointerEvent("pointerdown", { clientX: 60, clientY: 80, bubbles: true }));
    pad.dispatchEvent(new PointerEvent("pointerup", { clientX: 60, clientY: 80, bubbles: true }));

    expect(reg.get("a")).toBe(25);
    expect(reg.get("b")).toBe(0.5);
  });

  it("updates display labels with values and units", () => {
    const reg = createControlsRegistry();
    const xAxis = makeAxis({ name: "a", value: 42, unit: "ms", label: "Speed" });
    const yAxis = makeAxis({ name: "b", value: 7, label: "Bounce" });
    const { element } = createXYPad(xAxis, yAxis, reg, vi.fn());
    expect(element.textContent).toContain("Speed: 42 ms");
    expect(element.textContent).toContain("Bounce: 7");
  });

  it("cleanup does not throw", () => {
    const reg = createControlsRegistry();
    const { cleanup } = createXYPad(makeAxis({ name: "a" }), makeAxis({ name: "b" }), reg, vi.fn());
    expect(() => cleanup()).not.toThrow();
  });

  it("ignores pointer events when pad has zero size", () => {
    const reg = createControlsRegistry();
    const xAxis = makeAxis({ name: "a", value: 42, min: 0, max: 100 });
    const yAxis = makeAxis({ name: "b", value: 7, min: 0, max: 100 });
    const { element } = createXYPad(xAxis, yAxis, reg, vi.fn());
    const pad = element.querySelector(".veld-feedback-xy-pad") as HTMLElement;
    mockPadRect(pad, 0, 0); // zero size

    pad.dispatchEvent(new PointerEvent("pointerdown", { clientX: 50, clientY: 50, bubbles: true }));
    pad.dispatchEvent(new PointerEvent("pointerup", { clientX: 50, clientY: 50, bubbles: true }));

    // Values should remain at initial, not NaN
    expect(reg.get("a")).toBe(42);
    expect(reg.get("b")).toBe(7);
  });

  it("handles zero-range axis (min === max) without NaN", () => {
    const reg = createControlsRegistry();
    const xAxis = makeAxis({ name: "a", value: 50, min: 50, max: 50 }); // degenerate
    const yAxis = makeAxis({ name: "b", value: 0, min: 0, max: 100 });
    const { element } = createXYPad(xAxis, yAxis, reg, vi.fn());
    const pad = element.querySelector(".veld-feedback-xy-pad") as HTMLElement;
    mockPadRect(pad);

    // Dot position should be 50% (midpoint fallback), not NaN
    const dot = element.querySelector(".veld-feedback-xy-dot") as HTMLElement;
    expect(dot.style.left).toBe("50%");

    // Interaction should not produce NaN
    pad.dispatchEvent(new PointerEvent("pointerdown", { clientX: 100, clientY: 100, bubbles: true }));
    pad.dispatchEvent(new PointerEvent("pointerup", { clientX: 100, clientY: 100, bubbles: true }));
    expect(Number.isNaN(reg.get("a") as number)).toBe(false);
  });

  it("tracks pointer move during drag", () => {
    const reg = createControlsRegistry();
    const xAxis = makeAxis({ name: "a", min: 0, max: 100, step: 1, value: 0 });
    const yAxis = makeAxis({ name: "b", min: 0, max: 100, step: 1, value: 0 });
    const { element } = createXYPad(xAxis, yAxis, reg, vi.fn());
    const pad = element.querySelector(".veld-feedback-xy-pad") as HTMLElement;
    mockPadRect(pad);

    // Start drag
    pad.dispatchEvent(new PointerEvent("pointerdown", { clientX: 0, clientY: 200, bubbles: true }));
    expect(reg.get("a")).toBe(0);
    expect(reg.get("b")).toBe(0);

    // Move to center
    pad.dispatchEvent(new PointerEvent("pointermove", { clientX: 100, clientY: 100, bubbles: true }));
    expect(reg.get("a")).toBe(50);
    expect(reg.get("b")).toBe(50);

    // Release at top-right
    pad.dispatchEvent(new PointerEvent("pointerup", { clientX: 200, clientY: 0, bubbles: true }));
    expect(reg.get("a")).toBe(100);
    expect(reg.get("b")).toBe(100);
  });
});
