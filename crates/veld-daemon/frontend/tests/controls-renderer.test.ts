// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from "vitest";
import { parseControls, renderControls } from "../src/feedback-overlay/controls-renderer";
import { createControlsRegistry } from "../src/shared/controls";
import type { ControlDef } from "../src/shared/controls";

describe("parseControls", () => {
  it("returns null for plain text message", () => {
    expect(parseControls({ body: "just some text" })).toBeNull();
  });

  it("parses controls from <!--veld-controls--> marker", () => {
    const body = 'Try this:\n<!--veld-controls-->[{"type":"slider","name":"x","value":50,"min":0,"max":100}]';
    const result = parseControls({ body });
    expect(result).not.toBeNull();
    expect(result!.length).toBe(1);
    expect(result![0].type).toBe("slider");
    expect(result![0].name).toBe("x");
  });

  it("parses controls from controls property", () => {
    const controls: ControlDef[] = [{ type: "button", name: "go", label: "Go!" }];
    const result = parseControls({ body: "click below", controls });
    expect(result).not.toBeNull();
    expect(result![0].type).toBe("button");
  });

  it("prefers controls property over marker", () => {
    const controls: ControlDef[] = [{ type: "button", name: "a", label: "A" }];
    const body = 'text\n<!--veld-controls-->[{"type":"button","name":"b","label":"B"}]';
    const result = parseControls({ body, controls });
    expect(result![0].name).toBe("a"); // property wins
  });

  it("returns null for malformed JSON after marker", () => {
    const body = "text\n<!--veld-controls-->not json";
    expect(parseControls({ body })).toBeNull();
  });

  it("parses wrapped controls object", () => {
    const body = 'text\n<!--veld-controls-->{"controls":[{"type":"toggle","name":"x","value":true}]}';
    const result = parseControls({ body });
    expect(result).not.toBeNull();
    expect(result![0].type).toBe("toggle");
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
    const btn = element.querySelector("button") as HTMLButtonElement;
    // Find the Retry button (not the Apply button)
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
    // Should not throw
    expect(() => cleanup()).not.toThrow();
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
    // 2 buttons: "Go" + "Apply values"
    expect(element.querySelectorAll("button").length).toBe(2);
  });
});
