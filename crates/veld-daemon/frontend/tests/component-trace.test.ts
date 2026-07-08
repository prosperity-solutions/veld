// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { getComponentTrace, getComponentSource } from "../src/feedback-overlay/component-trace";

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
    // Depth-walking stops at MAX_DEPTH (100), but the trace returned to
    // callers is further capped to the last 12 entries (see next test) —
    // asserting the real number here, not just "under some large bound".
    expect(trace!.length).toBe(12);
  });

  it("caps a long trace to the last 12 entries, nearest the clicked element", () => {
    const el = document.createElement("div");
    // 20-entry chain: Comp0 is the outermost ancestor, Comp19 the element itself.
    let fiber: any = null;
    for (let i = 0; i < 20; i++) {
      fiber = { type: { name: `Comp${i}` }, return: fiber };
    }
    (el as any).__reactFiber$long = fiber;
    const trace = getComponentTrace(el);
    expect(trace).toEqual([
      "Comp8", "Comp9", "Comp10", "Comp11", "Comp12", "Comp13",
      "Comp14", "Comp15", "Comp16", "Comp17", "Comp18", "Comp19",
    ]);
  });

  it("returns a short trace unchanged (no padding, no truncation)", () => {
    const el = document.createElement("div");
    const fiber = { type: { name: "Only" }, return: null };
    (el as any).__reactFiber$short = fiber;
    expect(getComponentTrace(el)).toEqual(["Only"]);
  });
});

describe("getComponentSource", () => {
  it("returns null for an element with no framework data", () => {
    const el = document.createElement("div");
    expect(getComponentSource(el)).toBeNull();
  });

  it("reads React _debugSource off the element's own fiber", () => {
    const el = document.createElement("div");
    (el as any).__reactFiber$src = {
      type: { name: "Button" },
      _debugSource: { fileName: "src/components/Button.tsx", lineNumber: 42 },
      return: null,
    };
    expect(getComponentSource(el)).toEqual({ file: "src/components/Button.tsx", line: 42 });
  });

  it("walks up to the nearest ancestor fiber with _debugSource", () => {
    const el = document.createElement("div");
    (el as any).__reactFiber$src = {
      type: "div", // host node, no _debugSource of its own
      return: {
        type: { name: "Card" },
        _debugSource: { fileName: "src/components/Card.tsx", lineNumber: 7 },
        return: null,
      },
    };
    expect(getComponentSource(el)).toEqual({ file: "src/components/Card.tsx", line: 7 });
  });

  it("reads Vue's __file (no line number available)", () => {
    const el = document.createElement("div");
    (el as any).__vueParentComponent = {
      type: { __file: "src/components/Card.vue" },
      parent: null,
    };
    expect(getComponentSource(el)).toEqual({ file: "src/components/Card.vue" });
  });
});
