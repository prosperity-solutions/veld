// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from "vitest";
import { parseControls, renderControls } from "../src/feedback-overlay/controls-renderer";
import { createControlsRegistry } from "../src/shared/controls";
import type { ControlDef } from "../src/shared/controls";

describe("parseControls", () => {
  it("returns null for message without controls", () => {
    expect(parseControls({ body: "just some text" })).toBeNull();
  });

  it("returns controls from controls property", () => {
    const controls: ControlDef[] = [{ type: "button", name: "go", label: "Go!" }];
    const result = parseControls({ body: "click below", controls });
    expect(result).not.toBeNull();
    expect(result![0].type).toBe("button");
  });

  it("returns null when controls is empty array", () => {
    expect(parseControls({ body: "text", controls: [] })).toBeNull();
  });

  it("returns null when controls is undefined", () => {
    expect(parseControls({ body: "text", controls: undefined })).toBeNull();
  });
});

describe("renderControls", () => {
  let registry: ReturnType<typeof createControlsRegistry>;

  beforeEach(() => {
    registry = createControlsRegistry();
    // Mock fetch for Apply button
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, status: 200, json: () => Promise.resolve({}) }));
  });

  it("renders a slider control", () => {
    const controls: ControlDef[] = [
      { type: "slider", name: "vol", value: 50, min: 0, max: 100, label: "Volume" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const slider = element.querySelector("input[type=range]") as HTMLInputElement;
    expect(slider).not.toBeNull();
    expect(slider.value).toBe("50");
    expect(registry.get("vol")).toBe(50);
  });

  it("slider updates registry on input", () => {
    const controls: ControlDef[] = [
      { type: "slider", name: "vol", value: 50, min: 0, max: 100 },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const slider = element.querySelector("input[type=range]") as HTMLInputElement;
    slider.value = "75";
    slider.dispatchEvent(new Event("input"));
    expect(registry.get("vol")).toBe(75);
  });

  it("renders a number input", () => {
    const controls: ControlDef[] = [
      { type: "number", name: "dur", value: 200, min: 0, max: 2000, step: 10, unit: "ms" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const input = element.querySelector("input[type=number]") as HTMLInputElement;
    expect(input).not.toBeNull();
    expect(input.value).toBe("200");
  });

  it("renders a select dropdown", () => {
    const controls: ControlDef[] = [
      { type: "select", name: "ease", value: "ease-out", options: ["linear", "ease-in", "ease-out"] },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const select = element.querySelector("select") as HTMLSelectElement;
    expect(select).not.toBeNull();
    expect(select.value).toBe("ease-out");
    expect(select.options.length).toBe(3);
  });

  it("renders a color picker", () => {
    const controls: ControlDef[] = [
      { type: "color", name: "bg", value: "#ff0000" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const input = element.querySelector("input[type=color]") as HTMLInputElement;
    expect(input).not.toBeNull();
    expect(input.value).toBe("#ff0000");
  });

  it("renders a toggle", () => {
    const controls: ControlDef[] = [
      { type: "toggle", name: "on", value: true, label: "Enable" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const input = element.querySelector("input[type=checkbox]") as HTMLInputElement;
    expect(input).not.toBeNull();
    expect(input.checked).toBe(true);
  });

  it("renders a button", () => {
    const controls: ControlDef[] = [
      { type: "button", name: "retry", label: "Retry" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const buttons = element.querySelectorAll("button");
    const retryBtn = Array.from(buttons).find((b) => b.textContent === "Retry");
    expect(retryBtn).not.toBeUndefined();
  });

  it("button triggers action on registry", () => {
    const controls: ControlDef[] = [
      { type: "button", name: "replay", label: "Replay" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const cb = vi.fn();
    registry.onAction("replay", cb);
    const buttons = element.querySelectorAll("button");
    const replayBtn = Array.from(buttons).find((b) => b.textContent === "Replay");
    replayBtn!.click();
    expect(cb).toHaveBeenCalledOnce();
  });

  it("renders Apply button", () => {
    const controls: ControlDef[] = [
      { type: "number", name: "x", value: 1 },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const buttons = element.querySelectorAll("button");
    const applyBtn = Array.from(buttons).find((b) => b.textContent === "Apply values");
    expect(applyBtn).not.toBeUndefined();
  });

  it("cleanup removes scrub listeners", () => {
    const controls: ControlDef[] = [
      { type: "number", name: "x", value: 1, min: 0, max: 100 },
    ];
    const { cleanup } = renderControls(controls, registry, "t1");
    expect(() => cleanup()).not.toThrow();
  });

  it("renders a text input", () => {
    const controls: ControlDef[] = [
      { type: "text", name: "title", value: "Hello", placeholder: "Enter title" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const input = element.querySelector("input[type=text]") as HTMLInputElement;
    expect(input).not.toBeNull();
    expect(input.value).toBe("Hello");
    expect(input.placeholder).toBe("Enter title");
    expect(registry.get("title")).toBe("Hello");
  });

  it("text input updates registry on input", () => {
    const controls: ControlDef[] = [
      { type: "text", name: "title", value: "Hello" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    const input = element.querySelector("input[type=text]") as HTMLInputElement;
    input.value = "World";
    input.dispatchEvent(new Event("input"));
    expect(registry.get("title")).toBe("World");
  });

  it("skips select control with missing options array", () => {
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const controls = [
      { type: "select", name: "broken", value: "a" } as unknown as ControlDef,
      { type: "button", name: "ok", label: "OK" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    expect(element.querySelectorAll("select").length).toBe(0);
    expect(element.querySelectorAll("button").length).toBe(2);
    expect(warnSpy).toHaveBeenCalledWith(expect.stringContaining("broken"));
    warnSpy.mockRestore();
  });

  it("returns empty container for empty controls array", () => {
    const { element, cleanup } = renderControls([], registry, "t1");
    expect(element.children.length).toBe(0);
    expect(() => cleanup()).not.toThrow();
  });

  it("returns empty container for non-array controls", () => {
    const { element } = renderControls(null as unknown as ControlDef[], registry, "t1");
    expect(element.children.length).toBe(0);
  });

  it("renders multiple controls", () => {
    const controls: ControlDef[] = [
      { type: "slider", name: "a", value: 50, min: 0, max: 100 },
      { type: "select", name: "b", value: "x", options: ["x", "y"] },
      { type: "button", name: "c", label: "Go" },
    ];
    const { element } = renderControls(controls, registry, "t1");
    expect(element.querySelectorAll("input[type=range]").length).toBe(1);
    expect(element.querySelectorAll("select").length).toBe(1);
    expect(element.querySelectorAll("button").length).toBe(2);
  });

  describe("numeric control fusion (user-driven XY pad)", () => {
    const twoSliders: ControlDef[] = [
      { type: "slider", name: "duration", value: 200, min: 50, max: 2000, step: 10, unit: "ms", label: "Duration" },
      { type: "slider", name: "overshoot", value: 0, min: -1, max: 1, step: 0.01, label: "Overshoot" },
    ];

    it("numeric controls have a fuse grip handle", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const grips = element.querySelectorAll(".veld-feedback-control-fuse-grip");
      expect(grips.length).toBe(2);
    });

    it("fuse grip is draggable", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const grip = element.querySelector(".veld-feedback-control-fuse-grip") as HTMLElement;
      expect(grip.draggable).toBe(true);
    });

    it("dropping one numeric onto another creates XY pad", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const rows = element.querySelectorAll("[data-control-index]");
      const targetRow = rows[1] as HTMLElement;

      // Simulate drag-drop: source=0, target=1
      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" };
      targetRow.dispatchEvent(dropEvent);

      // XY pad should appear
      const pad = element.querySelector(".veld-feedback-xy-pad") as HTMLElement;
      expect(pad).not.toBeNull();
    });

    it("fused XY pad hides the original control rows", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const rows = element.querySelectorAll("[data-control-index]");
      const targetRow = rows[1] as HTMLElement;

      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" };
      targetRow.dispatchEvent(dropEvent);

      // Both original rows should be hidden
      expect((rows[0] as HTMLElement).style.display).toBe("none");
      expect((rows[1] as HTMLElement).style.display).toBe("none");
    });

    it("fused XY pad sets initial values in registry", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const targetRow = element.querySelectorAll("[data-control-index]")[1] as HTMLElement;

      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" };
      targetRow.dispatchEvent(dropEvent);

      expect(registry.get("duration")).toBe(200);
      expect(registry.get("overshoot")).toBe(0);
    });

    it("XY pad shows axis labels", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const targetRow = element.querySelectorAll("[data-control-index]")[1] as HTMLElement;

      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" };
      targetRow.dispatchEvent(dropEvent);

      expect(element.textContent).toContain("Duration");
      expect(element.textContent).toContain("Overshoot");
    });

    it("XY pad has a split button", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const targetRow = element.querySelectorAll("[data-control-index]")[1] as HTMLElement;

      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" };
      targetRow.dispatchEvent(dropEvent);

      const splitBtn = element.querySelector(".veld-feedback-xy-split-btn") as HTMLButtonElement;
      expect(splitBtn).not.toBeNull();
      expect(splitBtn.textContent).toBe("Split");
    });

    it("clicking split restores original controls", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const rows = element.querySelectorAll("[data-control-index]");
      const targetRow = rows[1] as HTMLElement;

      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" };
      targetRow.dispatchEvent(dropEvent);

      // Split
      const splitBtn = element.querySelector(".veld-feedback-xy-split-btn") as HTMLButtonElement;
      splitBtn.click();

      // XY pad removed
      expect(element.querySelector(".veld-feedback-xy-pad")).toBeNull();
      // Original rows visible again
      expect((rows[0] as HTMLElement).style.display).toBe("");
      expect((rows[1] as HTMLElement).style.display).toBe("");
    });

    it("XY pad updates registry on pointer interaction", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const targetRow = element.querySelectorAll("[data-control-index]")[1] as HTMLElement;

      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" };
      targetRow.dispatchEvent(dropEvent);

      const pad = element.querySelector(".veld-feedback-xy-pad") as HTMLElement;
      Object.defineProperty(pad, "getBoundingClientRect", {
        value: () => ({ left: 0, top: 0, width: 200, height: 200, right: 200, bottom: 200, x: 0, y: 0, toJSON() {} }),
      });

      pad.dispatchEvent(new PointerEvent("pointerdown", { clientX: 100, clientY: 100, bubbles: true }));
      pad.dispatchEvent(new PointerEvent("pointerup", { clientX: 100, clientY: 100, bubbles: true }));

      // Midpoint: x=(100/200)*(2000-50)+50=1025→snap10→1030, y=(1-100/200)*(1-(-1))+(-1)=0
      expect(registry.get("duration")).toBe(1030);
      expect(registry.get("overshoot")).toBe(0);
    });

    it("dropping onto self is a no-op", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const rows = element.querySelectorAll("[data-control-index]");
      const row0 = rows[0] as HTMLElement;

      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" }; // same index
      row0.dispatchEvent(dropEvent);

      // No pad created
      expect(element.querySelector(".veld-feedback-xy-pad")).toBeNull();
    });

    it("non-numeric controls have no fuse grip", () => {
      const controls: ControlDef[] = [
        { type: "text", name: "label", value: "hi" },
        { type: "toggle", name: "on", value: true },
      ];
      const { element } = renderControls(controls, registry, "t1");
      expect(element.querySelectorAll(".veld-feedback-control-fuse-grip").length).toBe(0);
    });

    it("number inputs without min/max have no fuse grip", () => {
      const controls: ControlDef[] = [
        { type: "number", name: "bare", value: 42 },
      ];
      const { element } = renderControls(controls, registry, "t1");
      expect(element.querySelectorAll(".veld-feedback-control-fuse-grip").length).toBe(0);
    });

    it("split then parent cleanup does not double-call pad cleanup", () => {
      const { element, cleanup } = renderControls(twoSliders, registry, "t1");
      const targetRow = element.querySelectorAll("[data-control-index]")[1] as HTMLElement;

      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "0" };
      targetRow.dispatchEvent(dropEvent);

      // Split first
      const splitBtn = element.querySelector(".veld-feedback-xy-split-btn") as HTMLButtonElement;
      splitBtn.click();

      // Then parent cleanup — should not throw from double-cleanup
      expect(() => cleanup()).not.toThrow();
    });

    it("dragover only highlights for veld control drags, not file drops", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const row = element.querySelectorAll("[data-control-index]")[0] as HTMLElement;

      // Simulate dragover from a file (no application/x-veld-control type)
      const dragOverEvent = new Event("dragover", { bubbles: true }) as any;
      dragOverEvent.preventDefault = vi.fn();
      dragOverEvent.dataTransfer = { types: ["Files"] };
      row.dispatchEvent(dragOverEvent);

      // Should NOT highlight — preventDefault not called, no drop-target class
      expect(dragOverEvent.preventDefault).not.toHaveBeenCalled();
      expect(row.classList.contains("veld-feedback-control-drop-target")).toBe(false);
    });

    it("reversed drag direction produces same fusion (normalized key)", () => {
      const { element } = renderControls(twoSliders, registry, "t1");
      const rows = element.querySelectorAll("[data-control-index]");

      // Drag 1 → 0 (reversed)
      const dropEvent = new Event("drop", { bubbles: true }) as any;
      dropEvent.preventDefault = vi.fn();
      dropEvent.dataTransfer = { getData: () => "1" };
      (rows[0] as HTMLElement).dispatchEvent(dropEvent);

      // Should still create exactly one pad
      expect(element.querySelectorAll(".veld-feedback-xy-pad").length).toBe(1);
      // Both rows hidden
      expect((rows[0] as HTMLElement).style.display).toBe("none");
      expect((rows[1] as HTMLElement).style.display).toBe("none");
    });
  });
});
