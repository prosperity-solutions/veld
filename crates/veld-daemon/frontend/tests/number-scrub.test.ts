// @vitest-environment jsdom
import { describe, it, expect, vi } from "vitest";
import { computeScrubValue, attachScrub } from "../src/shared/number-scrub";

describe("computeScrubValue", () => {
  it("increases value when dragging right", () => {
    expect(computeScrubValue(100, 10, 1, { min: 0, max: 1000, step: 1 })).toBe(110);
  });

  it("decreases value when dragging left", () => {
    expect(computeScrubValue(100, -20, 1, { min: 0, max: 1000, step: 1 })).toBe(80);
  });

  it("respects step size", () => {
    expect(computeScrubValue(100, 15, 1, { min: 0, max: 1000, step: 10 })).toBe(250);
  });

  it("clamps to min", () => {
    expect(computeScrubValue(10, -100, 1, { min: 0, max: 1000, step: 1 })).toBe(0);
  });

  it("clamps to max", () => {
    expect(computeScrubValue(990, 100, 1, { min: 0, max: 1000, step: 1 })).toBe(1000);
  });

  it("shift multiplier = 10x speed", () => {
    expect(computeScrubValue(100, 5, 10, { min: 0, max: 1000, step: 1 })).toBe(150);
  });

  it("ctrl multiplier = 0.1x precision", () => {
    expect(computeScrubValue(100, 10, 0.1, { min: 0, max: 1000, step: 1 })).toBe(101);
  });

  it("works with floating point step", () => {
    const result = computeScrubValue(1.0, 5, 1, { min: 0, max: 10, step: 0.1 });
    expect(result).toBeCloseTo(1.5);
  });

  it("no min/max means unbounded", () => {
    expect(computeScrubValue(0, -100, 1, { step: 1 })).toBe(-100);
    expect(computeScrubValue(0, 100, 1, { step: 1 })).toBe(100);
  });
});

describe("attachScrub (DOM integration)", () => {
  function makeInput(value = "100"): HTMLInputElement {
    const input = document.createElement("input");
    input.type = "number";
    input.value = value;
    // jsdom doesn't implement pointer capture — stub it
    input.setPointerCapture = vi.fn();
    input.releasePointerCapture = vi.fn();
    document.body.appendChild(input);
    return input;
  }

  it("Alt+pointerdown starts scrub, pointermove updates value", () => {
    const input = makeInput("100");
    const onChange = vi.fn();
    const cleanup = attachScrub(input, { min: 0, max: 1000, step: 1 }, onChange);

    // Alt+pointerdown
    input.dispatchEvent(new PointerEvent("pointerdown", {
      clientX: 200, altKey: true, bubbles: true,
    }));

    // Drag right 50px
    input.dispatchEvent(new PointerEvent("pointermove", {
      clientX: 250, bubbles: true,
    }));

    expect(onChange).toHaveBeenCalledWith(150);
    expect(input.value).toBe("150");

    // Release
    input.dispatchEvent(new PointerEvent("pointerup", { bubbles: true }));

    cleanup();
    input.remove();
  });

  it("pointerdown without Alt does NOT start scrub", () => {
    const input = makeInput("100");
    const onChange = vi.fn();
    const cleanup = attachScrub(input, { min: 0, max: 1000, step: 1 }, onChange);

    // No altKey
    input.dispatchEvent(new PointerEvent("pointerdown", {
      clientX: 200, altKey: false, bubbles: true,
    }));

    input.dispatchEvent(new PointerEvent("pointermove", {
      clientX: 250, bubbles: true,
    }));

    expect(onChange).not.toHaveBeenCalled();

    cleanup();
    input.remove();
  });

  it("calls setPointerCapture on drag start", () => {
    const input = makeInput("50");
    const cleanup = attachScrub(input, { min: 0, max: 100, step: 1 }, vi.fn());

    input.dispatchEvent(new PointerEvent("pointerdown", {
      clientX: 100, altKey: true, pointerId: 42, bubbles: true,
    }));

    expect(input.setPointerCapture).toHaveBeenCalledWith(42);

    input.dispatchEvent(new PointerEvent("pointerup", {
      pointerId: 42, bubbles: true,
    }));

    expect(input.releasePointerCapture).toHaveBeenCalledWith(42);

    cleanup();
    input.remove();
  });

  it("shift gives 10x multiplier during scrub", () => {
    const input = makeInput("100");
    const onChange = vi.fn();
    const cleanup = attachScrub(input, { min: 0, max: 2000, step: 1 }, onChange);

    input.dispatchEvent(new PointerEvent("pointerdown", {
      clientX: 200, altKey: true, bubbles: true,
    }));

    input.dispatchEvent(new PointerEvent("pointermove", {
      clientX: 210, shiftKey: true, bubbles: true,
    }));

    // 10px * step 1 * shift 10 = 100
    expect(onChange).toHaveBeenCalledWith(200);

    input.dispatchEvent(new PointerEvent("pointerup", { bubbles: true }));
    cleanup();
    input.remove();
  });

  it("cleanup removes all listeners", () => {
    const input = makeInput("50");
    const onChange = vi.fn();
    const cleanup = attachScrub(input, { min: 0, max: 100, step: 1 }, onChange);

    cleanup();

    // After cleanup, Alt+drag should do nothing
    input.dispatchEvent(new PointerEvent("pointerdown", {
      clientX: 100, altKey: true, bubbles: true,
    }));
    input.dispatchEvent(new PointerEvent("pointermove", {
      clientX: 150, bubbles: true,
    }));

    expect(onChange).not.toHaveBeenCalled();
    input.remove();
  });
});
