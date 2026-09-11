import { describe, expect, test } from "vitest";
import {
  DIALOG_NONE,
  escapeClosesDialog,
  isDialogOpen,
  pageChordsBlocked,
} from "./dialogGuards";

/**
 * Each case here names the bug it stands for. These five guards lived as
 * hand-written boolean expressions inside `AppInner`, three of them character
 * -for-character identical, and every one of them was written *after* the bug it
 * prevents had shipped — so the thing worth pinning is not the boolean, it is
 * that the boolean still covers the case somebody hit.
 */

/** Every `kind` the union carries, minus `none`. Kept as literals on purpose: */
/*  the predicates only ever compare against `none`, so any other string must   */
/*  behave identically, and listing the real ones says which strings are real.  */
const OPEN_KINDS = [
  "import",
  "new-worktree",
  "sharing",
  "rename",
  "trash",
  "confirm-delete",
  "update-main-dirty",
  "marker",
  "new-lane",
  "rename-lane",
  "move-lane-worktrees",
  "trash-lane-worktrees",
  "settings",
  "shortcuts",
  "remove-repo",
  "db-health",
  "search",
  "config-vars",
];

describe("isDialogOpen", () => {
  test("`none` is the only closed state", () => {
    expect(isDialogOpen({ kind: DIALOG_NONE })).toBe(false);
    for (const kind of OPEN_KINDS) {
      expect(isDialogOpen({ kind }), kind).toBe(true);
    }
  });

  test("DIALOG_NONE is the string the state actually stores", () => {
    // The one piece of coupling this module cannot check for itself: `App.tsx`
    // writes `{ kind: DIALOG_NONE }`, so this only has to stay the literal the
    // union declares. If the variant is ever renamed, renaming it here too is
    // what keeps every guard below honest — and forgetting fails *open*,
    // suppressing every guarded chord rather than throwing.
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
