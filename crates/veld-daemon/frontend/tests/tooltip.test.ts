// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from "vitest";
import { tipHtml, initTooltip, showTooltip, hideTooltip, attachTooltip } from "../src/feedback-overlay/tooltip";
import { initState } from "../src/feedback-overlay/state";
import { refs } from "../src/feedback-overlay/refs";

describe("tipHtml", () => {
  it("returns label only when no keys", () => {
    expect(tipHtml("Hello", [])).toBe("Hello");
  });

  it("builds kbd elements for each key", () => {
    const result = tipHtml("Screenshot", ["\u2318", "\u21E7", "S"]);
    expect(result).toContain("Screenshot");
    expect((result.match(/<kbd/g) || []).length).toBe(3);
    expect(result).toContain("\u2318");
    expect(result).toContain("\u21E7");
    expect(result).toContain("S");
  });

  it("wraps keys in kbd-group span", () => {
    const result = tipHtml("Test", ["A"]);
    expect(result).toContain("kbd-group");
  });

  it("handles null keys", () => {
    const result = tipHtml("Label", null as any);
    expect(result).toBe("Label");
  });
});

describe("tooltip lifecycle", () => {
  beforeEach(() => {
    const shadow = document.createElement("div").attachShadow({ mode: "open" });
    initState(shadow, document.createElement("div"));
    initTooltip();
  });

  it("initTooltip creates a tooltip element in shadow DOM", () => {
    expect(refs.tooltip).toBeTruthy();
    expect(refs.tooltip.tagName).toBe("DIV");
  });

  it("showTooltip makes it visible", () => {
    const anchor = document.createElement("button");
    document.body.appendChild(anchor);
    showTooltip(anchor, "Hello tooltip");
    expect(refs.tooltip.style.display).toBe("block");
    expect(refs.tooltip.innerHTML).toBe("Hello tooltip");
    document.body.removeChild(anchor);
  });

  it("hideTooltip hides it", () => {
    showTooltip(document.createElement("div"), "test");
    hideTooltip();
    expect(refs.tooltip.style.display).toBe("none");
  });

  it("attachTooltip wires mouseenter/mouseleave", () => {
    const btn = document.createElement("button");
    attachTooltip(btn, "Hover me");
    btn.dispatchEvent(new MouseEvent("mouseenter"));
    expect(refs.tooltip.style.display).toBe("block");
    btn.dispatchEvent(new MouseEvent("mouseleave"));
    expect(refs.tooltip.style.display).toBe("none");
  });
});
