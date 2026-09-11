import { describe, expect, test } from "vitest";
import {
  DIALOG_NONE,
  escapeClosesDialog,
  isDialogOpen,
  pageChordsBlocked,
} from "./dialogGuards";
import type { DialogKind } from "../App";

/**
 * Each case here names the bug it stands for. These five guards lived as
 * hand-written boolean expressions inside `AppInner`, three of them character
 * -for-character identical, and every one of them was written *after* the bug it
 * prevents had shipped — so the thing worth pinning is not the boolean, it is
 * that the boolean still covers the case somebody hit.
 */

/**
 * Every `kind` the union carries, minus `none`.
 *
 * A `Record` keyed by the union rather than an array of strings, because that
 * is what makes it **exhaustive by the compiler**: adding a 20th variant to
 * `DialogState` in `App.tsx` turns this into a type error until the variant is
 * listed here. As a hand-written array it compiled clean and the "every open
 * kind is true" test below simply stopped covering the new one — the test still
 * passes, and it silently means less. That is the same hand-maintained-pairing
 * failure the repo documents for the schema/example and shortcut-registry pairs.
 *
 * `import type` is erased at build time, so pulling this from `App.tsx` costs
 * the test nothing at runtime.
 */
const OPEN_KINDS_SET: Record<Exclude<DialogKind, typeof DIALOG_NONE>, true> = {
  import: true,
  "new-worktree": true,
  sharing: true,
  rename: true,
  trash: true,
  "confirm-delete": true,
  "update-main-dirty": true,
  marker: true,
  "new-lane": true,
  "rename-lane": true,
  "move-lane-worktrees": true,
  "trash-lane-worktrees": true,
  settings: true,
  shortcuts: true,
  "remove-repo": true,
  "db-health": true,
  search: true,
  "config-vars": true,
};

const OPEN_KINDS = Object.keys(OPEN_KINDS_SET);

test("this file runs in the `node` project, with no DOM", () => {
  // Pins the contract AGENTS.md states and `vite.config.ts` implements: a
  // `.test.ts` gets `environment: "node"`. Its twin is in
  // `shortcuts/ShortcutsDialog.test.tsx`. Together they are the only thing that
  // would notice a later global `environment: "jsdom"` — which is the change
  // this config deliberately does not make, for a cost measured in that file's
  // comment. Without these two, the doc and the 4.3s/11.2s rationale behind it
  // could go silently false.
  expect(typeof document).toBe("undefined");
});

describe("isDialogOpen", () => {
  test("`none` is the only closed state", () => {
    expect(isDialogOpen({ kind: DIALOG_NONE })).toBe(false);
    for (const kind of OPEN_KINDS) {
      expect(isDialogOpen({ kind }), kind).toBe(true);
    }
  });

  test("DIALOG_NONE is the string the state actually stores", () => {
    // `App.tsx` writes `{ kind: DIALOG_NONE }` against a union declaring the
    // literal `"none"`, so a rename of the variant is already a compile error
    // there. This covers the direction the compiler cannot: someone clearing
    // that error by restoring the literal at the write site instead of updating
    // the constant, which leaves every predicate comparing against a string the
    // state never holds.
    expect(DIALOG_NONE).toBe("none");
  });
});

describe("pageChordsBlocked", () => {
  test("nothing open — the chord acts", () => {
    expect(
      pageChordsBlocked({ dialogKind: DIALOG_NONE, promotionsOpen: false }),
    ).toBe(false);
  });

  test("a dialog is open — ⌃Tab, ⌥Tab, mod+shift+D and ⌘T/⌘W all stand down", () => {
    for (const kind of OPEN_KINDS) {
      expect(
        pageChordsBlocked({ dialogKind: kind, promotionsOpen: false }),
        kind,
      ).toBe(true);
    }
  });

  test("What's New is up, no dialog — still blocked", () => {
    // The half that gets forgotten, and the reason this takes two required
    // fields rather than one boolean. What's New is a second modal with its own
    // state and is *not* a `dialog` variant, so a guard reading only `dialog`
    // looks complete and is not: the release that adds a chord is the release
    // whose card announces it, so this is the overlay most likely to be up the
    // first time anyone presses the new chord. ⌘W closed a terminal tab behind
    // exactly that card.
    expect(
      pageChordsBlocked({ dialogKind: DIALOG_NONE, promotionsOpen: true }),
    ).toBe(true);
  });

  test("both open — blocked", () => {
    expect(
      pageChordsBlocked({ dialogKind: "settings", promotionsOpen: true }),
    ).toBe(true);
  });
});

describe("escapeClosesDialog", () => {
  test("Escape with a dialog open and no batch running — closes", () => {
    expect(
      escapeClosesDialog({
        key: "Escape",
        dialogKind: "settings",
        batchBusy: false,
      }),
    ).toBe(true);
  });

  test("Escape mid-batch does nothing at all", () => {
    // Not a style preference. Each batch dialog no-ops its own `onClose` while
    // its request loop runs, but Mantine's Escape listener is not the only one
    // — App binds a second on `window`, and that one closed over none of the
    // dialog's state and called `closeDialog()` regardless. So Escape mid-batch
    // unmounted the dialog while every remaining PATCH or DELETE kept firing.
    // Which reads as a cancel, and is not one.
    for (const kind of ["move-lane-worktrees", "trash-lane-worktrees"]) {
      expect(
        escapeClosesDialog({ key: "Escape", dialogKind: kind, batchBusy: true }),
        kind,
      ).toBe(false);
    }
  });

  test("Escape with nothing open does nothing", () => {
    expect(
      escapeClosesDialog({
        key: "Escape",
        dialogKind: DIALOG_NONE,
        batchBusy: false,
      }),
    ).toBe(false);
  });

  test("any other key is ignored, dialog open or not", () => {
    // The window listener sees every keydown in the app, so "only Escape" is a
    // real condition rather than a formality.
    for (const key of ["Enter", "Tab", "Escape ", "escape", "a", " "]) {
      expect(
        escapeClosesDialog({ key, dialogKind: "settings", batchBusy: false }),
        key,
      ).toBe(false);
    }
  });
});
