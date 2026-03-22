// @vitest-environment jsdom
import { describe, it, expect, beforeAll } from "vitest";

// jsdom doesn't implement CSS.escape — polyfill it
beforeAll(() => {
  if (typeof globalThis.CSS === "undefined") {
    (globalThis as any).CSS = {};
  }
  if (typeof CSS.escape !== "function") {
    CSS.escape = (s: string) => s.replace(/([^\w-])/g, "\\$1");
  }
});
import {
  mkEl,
  getThreadPageUrl,
  getThreadPosition,
  selectorFor,
} from "../src/feedback-overlay/helpers";
import type { Thread } from "../src/feedback-overlay/types";

function makeThread(overrides: Partial<Thread> = {}): Thread {
  return {
    id: "t1",
    scope: { type: "page", page_url: "/test" },
    status: "open",
    messages: [],
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString(),
    ...overrides,
  };
}

describe("mkEl", () => {
  it("creates element with correct tag", () => {
    expect(mkEl("div").tagName).toBe("DIV");
    expect(mkEl("span").tagName).toBe("SPAN");
    expect(mkEl("button").tagName).toBe("BUTTON");
  });

  it("prefixes all class names with veld-feedback-", () => {
    const el = mkEl("div", "toast");
    expect(el.className).toBe("veld-feedback-toast");
  });

  it("prefixes multiple space-separated classes", () => {
    const el = mkEl("div", "btn btn-primary btn-sm");
    expect(el.classList.contains("veld-feedback-btn")).toBe(true);
    expect(el.classList.contains("veld-feedback-btn-primary")).toBe(true);
    expect(el.classList.contains("veld-feedback-btn-sm")).toBe(true);
    expect(el.classList.length).toBe(3);
  });

  it("sets text content when provided", () => {
    expect(mkEl("p", undefined, "hello").textContent).toBe("hello");
  });

  it("handles empty string text (not omitted)", () => {
    const el = mkEl("div", "x", "");
    expect(el.textContent).toBe("");
  });

  it("omits class when undefined", () => {
    const el = mkEl("div");
    expect(el.className).toBe("");
  });
});

describe("selectorFor", () => {
  it("returns #id for element with id", () => {
    const el = document.createElement("div");
    el.id = "my-element";
    document.body.appendChild(el);
    expect(selectorFor(el)).toBe("#my-element");
    document.body.removeChild(el);
  });

  it("uses tag + class for element without id", () => {
    const el = document.createElement("div");
    el.className = "card highlighted";
    document.body.appendChild(el);
    const sel = selectorFor(el);
    expect(sel).toContain("div");
    expect(sel).toContain("card");
    document.body.removeChild(el);
  });

  it("uses nth-child for ambiguous siblings", () => {
    const parent = document.createElement("ul");
    const li1 = document.createElement("li");
    const li2 = document.createElement("li");
    parent.appendChild(li1);
    parent.appendChild(li2);
    document.body.appendChild(parent);
    const sel = selectorFor(li2);
    expect(sel).toContain(":nth-child(2)");
    document.body.removeChild(parent);
  });

  it("stops at #id ancestor", () => {
    const root = document.createElement("div");
    root.id = "root";
    const child = document.createElement("span");
    root.appendChild(child);
    document.body.appendChild(root);
    const sel = selectorFor(child);
    expect(sel).toMatch(/^#root > span/);
    document.body.removeChild(root);
  });

  it("filters out veld-feedback- prefixed classes", () => {
    const el = document.createElement("div");
    el.className = "veld-feedback-overlay real-class";
    document.body.appendChild(el);
    const sel = selectorFor(el);
    expect(sel).toContain("real-class");
    expect(sel).not.toContain("veld-feedback-overlay");
    document.body.removeChild(el);
  });
});

describe("getThreadPageUrl", () => {
  it("returns page_url from scope", () => {
    const t = makeThread({ scope: { type: "page", page_url: "/foo" } });
    expect(getThreadPageUrl(t)).toBe("/foo");
  });

  it("returns / as fallback for empty page_url", () => {
    const t = makeThread({ scope: { type: "page", page_url: "" } });
    expect(getThreadPageUrl(t)).toBe("/");
  });
});

describe("getThreadPosition", () => {
  it("returns position when present", () => {
    const t = makeThread({
      scope: {
        type: "element",
        page_url: "/",
        position: { x: 10, y: 20, width: 100, height: 50 },
      },
    });
    expect(getThreadPosition(t)).toEqual({ x: 10, y: 20, width: 100, height: 50 });
  });

  it("returns null for page-scoped thread", () => {
    const t = makeThread({ scope: { type: "page", page_url: "/" } });
    expect(getThreadPosition(t)).toBeNull();
  });
});
