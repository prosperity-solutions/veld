import { describe, it, expect } from "vitest";
import { tipHtml } from "../src/feedback-overlay/tooltip";

describe("tipHtml", () => {
  it("returns label only when no keys", () => {
    expect(tipHtml("Hello", [])).toBe("Hello");
  });

  it("returns label with kbd elements for keys", () => {
    const result = tipHtml("Screenshot", ["⌘", "⇧", "S"]);
    expect(result).toContain("Screenshot");
    expect(result).toContain("<kbd");
    expect(result).toContain("⌘");
    expect(result).toContain("⇧");
    expect(result).toContain("S");
  });

  it("wraps keys in kbd-group span", () => {
    const result = tipHtml("Test", ["A"]);
    expect(result).toContain("kbd-group");
    expect(result).toContain("<kbd");
  });

  it("handles single key", () => {
    const result = tipHtml("Undo", ["Z"]);
    expect(result).toContain("Z");
    expect(result.match(/<kbd/g)?.length).toBe(1);
  });
});
