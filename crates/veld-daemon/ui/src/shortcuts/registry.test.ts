import { describe, expect, it } from "vitest";
import {
  comboTokens,
  duplicateShortcutIds,
  nextIndex,
  SHORTCUTS,
  shortcutProblems,
} from "./registry";

describe("SHORTCUTS", () => {
  it("has no malformed entries", () => {
    const problems = SHORTCUTS.flatMap(shortcutProblems);
    expect(problems).toEqual([]);
  });

  it("has no duplicate ids", () => {
    expect(duplicateShortcutIds(SHORTCUTS)).toEqual([]);
  });

  // The registry is the single source of truth for the overview dialog — see
  // its own doc comment. This is a floor, not a drift gate: it cannot see
  // whether `App.tsx`'s keydown effect still agrees with what is listed here,
  // only that the list itself has not shrunk back to what shipped before this
  // change.
  it("covers at least the shortcuts this change added", () => {
    const ids = new Set(SHORTCUTS.map((s) => s.id));
    for (const id of [
      "navigate-worktrees",
      "switch-tab",
      "focus-mode",
      "switch-view",
      "update-main",
      "select-preset",
      "start-stop",
      "restart-run",
      "open-shortcuts",
      "find-in-page",
      "new-window",
      "insert-newline",
    ]) {
      expect(ids.has(id)).toBe(true);
    }
  });
});

describe("nextIndex", () => {
  it("steps forward and backward inside a normal cycle", () => {
    expect(nextIndex(1, 1, 5)).toBe(2);
    expect(nextIndex(1, -1, 5)).toBe(0);
  });

  it("wraps at both ends", () => {
    expect(nextIndex(4, 1, 5)).toBe(0);
    expect(nextIndex(0, -1, 5)).toBe(4);
  });

  it("lands on the first entry stepping forward from nothing focused", () => {
    expect(nextIndex(-1, 1, 5)).toBe(0);
  });

  it("lands on the LAST entry stepping backward from nothing focused — the off-by-one a bare modulo gets wrong", () => {
    // `((-1 - 1) % 5 + 5) % 5` is 3 (second-to-last), not 4 (last) — this is
    // the case `nextIndex` exists to get right explicitly.
    expect(nextIndex(-1, -1, 5)).toBe(4);
  });

  it("is -1 for an empty list", () => {
    expect(nextIndex(-1, 1, 0)).toBe(-1);
  });
});

describe("comboTokens", () => {
  it("renders the Mac glyphs", () => {
    expect(comboTokens({ mod: true, shift: true, keys: ["F"] }, true)).toEqual([
      "⌘",
      "⇧",
      "F",
    ]);
  });

  it("renders Ctrl/Shift on every other platform", () => {
    expect(comboTokens({ mod: true, shift: true, keys: ["F"] }, false)).toEqual([
      "Ctrl",
      "Shift",
      "F",
    ]);
  });

  it("keeps a literal ctrl distinct from mod", () => {
    // Tab-switching binds the physical Ctrl key on every platform, Mac
    // included — see the `KeyCombo` doc comment.
    expect(comboTokens({ ctrl: true, keys: ["Tab"] }, true)).toEqual(["Ctrl", "Tab"]);
  });
});
