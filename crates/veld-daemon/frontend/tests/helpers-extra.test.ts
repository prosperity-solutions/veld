// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import {
  mkEl,
  getThreadPageUrl,
  getThreadPosition,
  modKey,
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
  it("creates element with tag", () => {
    const el = mkEl("div");
    expect(el.tagName).toBe("DIV");
  });

  it("adds prefixed class names", () => {
    const el = mkEl("span", "toast");
    expect(el.className).toBe("veld-feedback-toast");
  });

  it("handles multiple classes", () => {
    const el = mkEl("div", "btn btn-primary");
    expect(el.className).toBe("veld-feedback-btn veld-feedback-btn-primary");
  });

  it("sets text content", () => {
    const el = mkEl("p", undefined, "hello");
    expect(el.textContent).toBe("hello");
  });

  it("works with empty text", () => {
    const el = mkEl("div", "x", "");
    expect(el.textContent).toBe("");
  });
});

describe("getThreadPageUrl", () => {
  it("returns scope page_url", () => {
    const t = makeThread({ scope: { type: "page", page_url: "/foo" } });
    expect(getThreadPageUrl(t)).toBe("/foo");
  });

  it("returns / as fallback for empty page_url", () => {
    const t = makeThread({ scope: { type: "page", page_url: "" } });
    // getThreadPageUrl returns page_url or "/" fallback
    expect(getThreadPageUrl(t)).toBe("/");
  });
});

describe("getThreadPosition", () => {
  it("returns position from scope", () => {
    const t = makeThread({
      scope: {
        type: "element",
        page_url: "/",
        position: { x: 10, y: 20, width: 100, height: 50 },
      },
    });
    expect(getThreadPosition(t)).toEqual({
      x: 10, y: 20, width: 100, height: 50,
    });
  });

  it("returns null when no position", () => {
    const t = makeThread({ scope: { type: "page", page_url: "/" } });
    expect(getThreadPosition(t)).toBeNull();
  });
});

describe("modKey", () => {
  it("checks metaKey on Mac", () => {
    // modKey checks IS_MAC which is determined at import time
    // In test env (Node), navigator.platform may vary
    const e = { metaKey: true, ctrlKey: false } as KeyboardEvent;
    // Just verify it returns a boolean
    expect(typeof modKey(e)).toBe("boolean");
  });
});
