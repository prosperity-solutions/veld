// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { getComponentTrace } from "../src/feedback-overlay/component-trace";

describe("getComponentTrace", () => {
  it("returns null for plain HTML element", () => {
    const el = document.createElement("div");
    expect(getComponentTrace(el)).toBeNull();
  });

  it("returns null for element with no framework data", () => {
    const el = document.createElement("span");
    el.id = "test";
    el.className = "foo bar";
    expect(getComponentTrace(el)).toBeNull();
  });

  it("detects React fiber and extracts component names", () => {
    const el = document.createElement("div");
    // Simulate React fiber
    const fiber = {
      type: { displayName: "MyComponent" },
      return: {
        type: { name: "ParentComponent" },
        return: {
          type: { name: "App" },
          return: null,
        },
      },
    };
    (el as any).__reactFiber$test123 = fiber;
    const trace = getComponentTrace(el);
    expect(trace).not.toBeNull();
    expect(trace).toContain("MyComponent");
    expect(trace).toContain("ParentComponent");
    expect(trace).toContain("App");
  });

  it("skips native HTML element fibers (string type)", () => {
    const el = document.createElement("div");
    const fiber = {
      type: "div", // string = native element, not a component
      return: {
        type: { name: "RealComponent" },
        return: null,
      },
    };
    (el as any).__reactFiber$xyz = fiber;
    const trace = getComponentTrace(el);
    expect(trace).not.toBeNull();
    expect(trace).toEqual(["RealComponent"]);
  });

  it("detects Vue 3 component chain", () => {
    const el = document.createElement("div");
    (el as any).__vueParentComponent = {
      type: { name: "ChildComponent" },
      parent: {
        type: { __name: "ParentComponent" },
        parent: null,
      },
    };
    const trace = getComponentTrace(el);
    expect(trace).not.toBeNull();
    expect(trace).toContain("ChildComponent");
    expect(trace).toContain("ParentComponent");
  });

  it("detects Vue 2 instance chain", () => {
    const el = document.createElement("div");
    (el as any).__vue__ = {
      $options: { name: "MyVue2Component" },
      $parent: {
        $options: { name: "RootComponent" },
        $parent: null,
      },
    };
    const trace = getComponentTrace(el);
    expect(trace).not.toBeNull();
    expect(trace).toContain("MyVue2Component");
    expect(trace).toContain("RootComponent");
  });

  it("limits depth to prevent infinite loops", () => {
    const el = document.createElement("div");
    // Create a very deep chain
    let fiber: any = { type: { name: "Deep" }, return: null };
    for (let i = 0; i < 200; i++) {
      fiber = { type: { name: `Level${i}` }, return: fiber };
    }
    (el as any).__reactFiber$deep = fiber;
    const trace = getComponentTrace(el);
    expect(trace).not.toBeNull();
    // Should be capped at MAX_DEPTH (100)
    expect(trace!.length).toBeLessThanOrEqual(100);
  });
});
