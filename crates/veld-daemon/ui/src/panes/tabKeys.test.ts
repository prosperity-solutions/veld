import { describe, expect, it } from "vitest";
import { tabKeyAction } from "./tabKeys";

describe("tabKeyAction", () => {
  it("moves focus one tab at a time", () => {
    expect(tabKeyAction("ArrowRight", 0, 3)).toEqual({ kind: "focus", index: 1 });
    expect(tabKeyAction("ArrowLeft", 2, 3)).toEqual({ kind: "focus", index: 1 });
  });

  it("wraps at both ends", () => {
    expect(tabKeyAction("ArrowRight", 2, 3)).toEqual({ kind: "focus", index: 0 });
    expect(tabKeyAction("ArrowLeft", 0, 3)).toEqual({ kind: "focus", index: 2 });
  });

  it("jumps to the ends", () => {
    expect(tabKeyAction("Home", 2, 3)).toEqual({ kind: "focus", index: 0 });
    expect(tabKeyAction("End", 0, 3)).toEqual({ kind: "focus", index: 2 });
  });

  it("stays put in a strip of one", () => {
    for (const key of ["ArrowRight", "ArrowLeft", "Home", "End"]) {
      expect(tabKeyAction(key, 0, 1)).toEqual({ kind: "focus", index: 0 });
    }
  });

  it("closes on Delete and Backspace, whatever the strip looks like", () => {
    expect(tabKeyAction("Delete", 0, 3)).toEqual({ kind: "close" });
    expect(tabKeyAction("Backspace", 2, 3)).toEqual({ kind: "close" });
    // A strip of one is exactly when closing by keyboard matters, and it must not
    // fall foul of a bounds check meant for movement.
    expect(tabKeyAction("Delete", 0, 1)).toEqual({ kind: "close" });
  });

  it("ignores keys that belong to somebody else", () => {
    // Enter and Space are the button's own — they select. Everything else is the
    // app's (⌘K, ⌘W, a terminal's readline bindings).
    for (const key of ["Enter", " ", "k", "Escape", "Tab", "ArrowDown"]) {
      expect(tabKeyAction(key, 0, 3)).toEqual({ kind: "ignore" });
    }
  });

  it("ignores movement when the focused index is not in the strip", () => {
    // Reachable while a drag is mid-flight: the tab that had focus can be gone
    // from this dock's strip by the time a key arrives.
    expect(tabKeyAction("ArrowRight", 5, 3)).toEqual({ kind: "ignore" });
    expect(tabKeyAction("ArrowRight", -1, 3)).toEqual({ kind: "ignore" });
    expect(tabKeyAction("Home", 0, 0)).toEqual({ kind: "ignore" });
  });
});
