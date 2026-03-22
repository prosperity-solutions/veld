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
});
