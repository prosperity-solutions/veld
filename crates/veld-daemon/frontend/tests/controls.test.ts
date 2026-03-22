import { describe, it, expect, vi } from "vitest";
import { createControlsRegistry } from "../src/shared/controls";

describe("VeldControls registry", () => {
  it("get returns undefined for unset values", () => {
    const reg = createControlsRegistry();
    expect(reg.get("missing")).toBeUndefined();
  });

  it("set + get roundtrip", () => {
    const reg = createControlsRegistry();
    reg.set("duration", 200);
    expect(reg.get("duration")).toBe(200);
  });

  it("set overwrites previous value", () => {
    const reg = createControlsRegistry();
    reg.set("x", 1);
    reg.set("x", 2);
    expect(reg.get("x")).toBe(2);
  });

  it("on() fires callback when value changes", () => {
    const reg = createControlsRegistry();
    const cb = vi.fn();
    reg.on("duration", cb);
    reg.set("duration", 300);
    expect(cb).toHaveBeenCalledWith(300);
  });

  it("on() does NOT fire for other names", () => {
    const reg = createControlsRegistry();
    const cb = vi.fn();
    reg.on("duration", cb);
    reg.set("easing", "linear");
    expect(cb).not.toHaveBeenCalled();
  });

  it("on() fires on every set", () => {
    const reg = createControlsRegistry();
    const cb = vi.fn();
    reg.on("x", cb);
    reg.set("x", 1);
    reg.set("x", 2);
    reg.set("x", 3);
    expect(cb).toHaveBeenCalledTimes(3);
    expect(cb).toHaveBeenLastCalledWith(3);
  });

  it("on() returns unsubscribe function", () => {
    const reg = createControlsRegistry();
    const cb = vi.fn();
    const unsub = reg.on("x", cb);
    reg.set("x", 1);
    expect(cb).toHaveBeenCalledTimes(1);
    unsub();
    reg.set("x", 2);
    expect(cb).toHaveBeenCalledTimes(1); // not called again
  });

  it("multiple listeners on same name", () => {
    const reg = createControlsRegistry();
    const cb1 = vi.fn();
    const cb2 = vi.fn();
    reg.on("x", cb1);
    reg.on("x", cb2);
    reg.set("x", 42);
    expect(cb1).toHaveBeenCalledWith(42);
    expect(cb2).toHaveBeenCalledWith(42);
  });

  it("onAction/trigger for button events", () => {
    const reg = createControlsRegistry();
    const cb = vi.fn();
    reg.onAction("retry", cb);
    reg.trigger("retry");
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("onAction returns unsubscribe", () => {
    const reg = createControlsRegistry();
    const cb = vi.fn();
    const unsub = reg.onAction("stop", cb);
    reg.trigger("stop");
    unsub();
    reg.trigger("stop");
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("trigger without listener doesn't crash", () => {
    const reg = createControlsRegistry();
    expect(() => reg.trigger("unknown")).not.toThrow();
  });

  it("values() returns all current values", () => {
    const reg = createControlsRegistry();
    reg.set("a", 1);
    reg.set("b", "hello");
    reg.set("c", true);
    expect(reg.values()).toEqual({ a: 1, b: "hello", c: true });
  });

  it("values() returns a copy (not mutable reference)", () => {
    const reg = createControlsRegistry();
    reg.set("x", 1);
    const v = reg.values();
    v["x"] = 999;
    expect(reg.get("x")).toBe(1); // unchanged
  });

  it("supports any value type", () => {
    const reg = createControlsRegistry();
    reg.set("num", 42);
    reg.set("str", "hello");
    reg.set("bool", true);
    reg.set("arr", [1, 2, 3]);
    reg.set("obj", { a: 1 });
    expect(reg.get("num")).toBe(42);
    expect(reg.get("str")).toBe("hello");
    expect(reg.get("bool")).toBe(true);
    expect(reg.get("arr")).toEqual([1, 2, 3]);
    expect(reg.get("obj")).toEqual({ a: 1 });
  });

  it("wildcard listener fires on any change", () => {
    const reg = createControlsRegistry();
    const cb = vi.fn();
    reg.on("*", cb);
    reg.set("x", 1);
    reg.set("y", 2);
    expect(cb).toHaveBeenCalledTimes(2);
    expect(cb).toHaveBeenNthCalledWith(1, 1);
    expect(cb).toHaveBeenNthCalledWith(2, 2);
  });
});
