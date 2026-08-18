import { describe, expect, it } from "vitest";
import {
  comboTokens,
  combosFor,
  duplicateShortcutIds,
  nextIndex,
  SHORTCUTS,
  shortcutProblems,
} from "./registry";
import { handleKeyEvent } from "../panes/terminalKeys";

/** The registry renders display tokens; `isAppShortcutChord` matches `e.key`.
 *  Only the tokens that are not simply their own lowercase need a mapping. */
const KEY_FOR_TOKEN: Record<string, string> = {
  "↑": "ArrowUp",
  "↓": "ArrowDown",
  "←": "ArrowLeft",
  "→": "ArrowRight",
  Enter: "Enter",
  Tab: "Tab",
};

describe("SHORTCUTS", () => {
  it("has no malformed entries", () => {
    const problems = SHORTCUTS.flatMap(shortcutProblems);
    expect(problems).toEqual([]);
  });

  it("has no duplicate ids", () => {
    expect(duplicateShortcutIds(SHORTCUTS)).toEqual([]);
  });

  it("treats a missing description as valid but an empty one as a mistake", () => {
    const base = { id: "x", category: "general" as const, title: "X", combos: [{ keys: ["X"] }] };
    expect(shortcutProblems(base)).toEqual([]);
    expect(shortcutProblems({ ...base, description: "" })).toEqual([
      "x: description must not be an empty string — omit it instead",
    ]);
    expect(shortcutProblems({ ...base, description: "A sentence." })).toEqual([]);
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
      "cycle-tabs",
      "new-tab",
      "close-tab",
      "close-window",
      "split-dock",
    ]) {
      expect(ids.has(id)).toBe(true);
    }
  });

  // The veld feedback overlay (`feedback-overlay/keyboard.ts`) binds
  // mod+Shift+{V, ., F, S, P, C} on a capture-phase listener that always wins
  // a shared chord — this is why focus mode and the view switch moved to
  // L/X. A regression here is exactly how that collision shipped the first
  // time: nothing checked a new mod+shift letter against the overlay's list.
  it("keeps every mod+shift single-letter combo off the feedback overlay's own chords", () => {
    const overlayLetters = new Set(["V", "F", "S", "P", "C"]);
    for (const s of SHORTCUTS) {
      for (const combo of s.combos) {
        if (!combo.mod || !combo.shift || combo.keys.length !== 1) continue;
        const letter = combo.keys[0];
        expect(overlayLetters.has(letter), `${s.id} uses mod+shift+${letter}`).toBe(false);
      }
    }
  });

  // **The drift gate AGENTS.md's checklist warns about, made real.**
  //
  // A mod+shift chord that is dispatched from `App.tsx`'s keydown effect only
  // reaches that listener if `isAppShortcutChord` lets it past xterm first —
  // and nothing tied the two together, so the third place was "easy to miss"
  // by hand on every change. It was in fact missed once already, on the diff
  // that introduced the checklist row.
  //
  // Exempt by an **explicit list of menu accelerators**, never by
  // `desktopOnly`. Those are different facts and conflating them silently
  // shrinks the gate: `restart-run` (⌘⇧K) and `select-preset` (⌘⇧O) are
  // `desktopOnly` because *browsers* reserve those chords, but both are
  // dispatched from `App.tsx`'s keydown effect and both depend on `isAppShortcutChord`
  // — exempting them would leave exactly the drift this test exists to catch.
  // `desktopOnly` means "unreachable in a browser tab"; what matters here is
  // "handled by the Electron menu before web contents see the key".
  //
  // `handleKeyEvent` returning `false` is exactly "xterm ignores this without
  // cancelling it", i.e. it keeps bubbling to the window listener.
  // **Closed-world**: every row must reach the window listener from a focused
  // terminal *unless it is named here with a reason*. An allow-list keyed on
  // chord shape silently shrinks whenever a chord changes shape — that already
  // happened twice on this diff, once when navigation moved off `mod+shift` and
  // again when it became literal `ctrl`/`alt`, and both times the gate went
  // quiet instead of failing.
  const NOT_IN_TERMINALS: Record<string, string> = {
    // Menu accelerators — handled before web contents, xterm included.
    "new-tab": "menu accelerator",
    "close-tab": "menu accelerator",
    "close-window": "menu accelerator",
    "new-window": "menu accelerator",
    "find-in-page": "per-pane accelerator, forwarded from browserViews.js",
    // Ctrl+K is readline's kill-to-end-of-line, Ctrl+B its back-one-character:
    // both belong to the shell, so these are reachable from a terminal via ⌘ only.
    "command-palette": "Ctrl+K belongs to readline",
    "toggle-project-column": "Ctrl+B belongs to readline",
    // ⌘/ does pass; the literal Ctrl+/ is readline's undo, so the ctrl arm
    // fails by design.
    "open-shortcuts": "Ctrl+/ is readline's undo",
    // "1…9" is a display token rather than one key, and the digits are
    // forwarded out of browser panes rather than let past xterm.
    "switch-project": "display token, not a single key",
    "previous-project": "not claimed in a terminal",
    // Escape is xterm's: vim, less and every TUI menu need it.
    "close-dialog": "Escape belongs to the terminal",
  };
  it("lets every page-dispatched modified chord past a focused terminal", () => {
    for (const s of SHORTCUTS) {
      if (NOT_IN_TERMINALS[s.id]) continue;
      for (const combo of s.combos) {
        if (combo.keys.length !== 1) continue;
        // **Any modifier, not just `mod`.** Keying this on `combo.mod` let the
        // Tab-shaped navigation family — which uses literal `ctrl`/`alt` — drop
        // out of the gate silently, which is the precise failure this test
        // exists to prevent, committed by the test itself.
        if (!combo.mod && !combo.shift && !combo.alt && !combo.ctrl) continue;
        const key = KEY_FOR_TOKEN[combo.keys[0]] ?? combo.keys[0].toLowerCase();
        // A combo with no `mod` is bound to literal keys, so it has exactly one
        // spelling; `mod` ones must survive both.
        const mods = combo.mod ? (["ctrlKey", "metaKey"] as const) : (["none"] as const);
        for (const mod of mods) {
          const event = {
            type: "keydown",
            key,
            // `code` matters for the named keys: `isShiftEnter` matches on it,
            // so leaving it empty made Shift+Enter look unhandled.
            code: key,
            shiftKey: combo.shift === true,
            altKey: combo.alt === true,
            ctrlKey: mod === "ctrlKey" || combo.ctrl === true,
            metaKey: mod === "metaKey",
            preventDefault: () => {},
          } as unknown as KeyboardEvent;
          expect(
            handleKeyEvent(event, () => {}),
            `${s.id} (${combo.keys[0]}, ${mod}) is swallowed by a focused terminal — ` +
              "add it to isAppShortcutChord in panes/terminalKeys.ts, or to " +
              "NOT_IN_TERMINALS here with the reason",
          ).toBe(false);
        }
      }
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
    // For the chord that binds the physical Ctrl key on every platform, Mac
    // included, rather than the cross-platform `mod` accelerator — see the
    // `KeyCombo` doc comment.
    expect(comboTokens({ ctrl: true, keys: ["Tab"] }, true)).toEqual(["Ctrl", "Tab"]);
  });

});

describe("one action, one shortcut", () => {
  // A deliberate rule, not an accident of the current list: aliases were tried
  // (⌘↑/↓ with ←/→ doing the same thing, a second accelerator for the palette)
  // and removed, because a second way to do one thing is a chord spent on
  // nothing and a row the overview has to explain. A row may still carry two
  // combos — but only as the two *directions* of one pair, never two spellings
  // of one direction.
  it("gives no action two spellings of the same direction, on either platform", () => {
    // **Checked per platform**, because a row may legitimately carry four
    // combos — two for macOS and two for everywhere else — when the chord
    // itself differs rather than just the modifier's name. Worktree navigation
    // is the one such row. A reader still only ever sees two, so the rule the
    // maintainer set ("one action = one shortcut") holds where it matters:
    // on the screen in front of them.
    for (const s of SHORTCUTS) {
      for (const mac of [true, false]) {
        const mine = combosFor(s, mac);
        const rendered = mine.map((c) => comboTokens(c, mac).join(" "));
        expect(new Set(rendered).size, `${s.id} lists a combo twice on ${mac ? "mac" : "other"}`).toBe(
          rendered.length,
        );
        expect(
          mine.length,
          `${s.id} offers ${mine.length} combos on ${mac ? "mac" : "other"} — a row is one ` +
            "action, or one previous/next pair, never a list of aliases",
        ).toBeLessThanOrEqual(2);
        expect(mine.length, `${s.id} has no combo at all on ${mac ? "mac" : "other"}`).toBeGreaterThan(0);
      }
    }
  });
});
