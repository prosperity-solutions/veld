/**
 * When an open modal takes a keyboard chord away from the page underneath it.
 *
 * Five places in `App.tsx` ask a version of "is anything open" for a reason that
 * has nothing to do with rendering, and every one of them is guarding a bug this
 * app has already shipped once:
 *
 * - **`⌘W` closed a terminal tab behind the Settings modal**, with no
 *   confirmation, on the reflex Chrome parity had just trained.
 * - **`Escape` mid-batch unmounted the dialog while every remaining `PATCH` or
 *   `DELETE` kept firing.** Which reads as a cancel, and is not one.
 * - **`⌃Tab` cycled the tab strip invisibly behind the What's New card** — the
 *   card that existed to announce `⌃Tab`.
 *
 * They lived as five hand-written boolean expressions inside a 6,100-line
 * component, where nothing could test them and the only way to find a wrong one
 * was to read it. Hence this module: the expressions, as named predicates, with
 * no React and no DOM in them.
 *
 * **The half that gets forgotten is `promotionsOpen`.** What's New is a second
 * modal with its own state — it is *not* a `dialog` variant — so a guard written
 * as "is `dialog` open" looks complete, reads correctly, and still lets a chord
 * through the one overlay most likely to be up: the release that adds a chord is
 * the release whose card announces it. Taking both halves as required fields of
 * one argument is the point of `pageChordsBlocked`; a new chord's author cannot
 * write half of it.
 *
 * Scope: this is about *page-dispatched* chords and the Electron tab commands.
 * Which chord belongs to which mechanism is `shortcuts/registry.ts`'s subject,
 * and whether a terminal pane lets a chord reach the window at all is
 * `panes/terminalKeys.ts`'s.
 */

/**
 * The `dialog` state's closed value, and the string every predicate here
 * compares against.
 *
 * Exported so the state and the predicates share one spelling. Note what does
 * and does not protect that: `App.tsx` writes `{ kind: DIALOG_NONE }` against a
 * union that declares the literal `"none"`, so **renaming the variant is a
 * compile error at both write sites** — the compiler catches it before any
 * predicate can misbehave. What is not checked is the direction nobody expects:
 * "fixing" that error by putting the literal back at the write site rather than
 * updating this constant, which would leave the predicates comparing against a
 * string the state never holds — reporting "open" forever, and so suppressing
 * every guarded chord rather than throwing.
 */
export const DIALOG_NONE = "none" as const;

/**
 * Only the discriminant, never a payload. The real state is a 19-variant union
 * in `App.tsx`; nothing here needs to know more than whether it is `none`, and
 * accepting the shape structurally keeps that union where it is.
 */
export type DialogLike = { readonly kind: string };

/** Whether a `dialog` state value represents an open dialog. */
export function isDialogOpen(dialog: DialogLike): boolean {
  return dialog.kind !== DIALOG_NONE;
}

/**
 * Whether an overlay is up, and a page-dispatched chord must therefore not act.
 *
 * Used by `⌃Tab`/`⌥Tab` worktree and tab navigation, by `mod+shift+D`, and by
 * the Electron `⌘T`/`⌘W` tab commands. The last of those is the reason this is
 * not simply inlined: a menu accelerator is handled *before* web contents see
 * the key and cannot be made conditional, so its guard has to be written by
 * hand in the renderer — and writing it by hand is what went wrong.
 */
export function pageChordsBlocked(state: {
  /** `dialog.kind`, usually read off a ref in a window listener. */
  dialogKind: string;
  /** Whether the What's New modal is up (`promotions.open !== null`). */
  promotionsOpen: boolean;
}): boolean {
  return state.dialogKind !== DIALOG_NONE || state.promotionsOpen;
}

/**
 * Whether this keydown should close the open dialog.
 *
 * Mantine's `Modal` has its own Escape listener, and `App.tsx` binds a second
 * one on `window`. That second one is the problem this encodes: each batch
 * dialog no-ops its own `onClose` while its request loop runs, but the window
 * listener closed over none of that and called `closeDialog()` regardless — so
 * Escape mid-batch unmounted the dialog while the remaining requests carried on.
 * `batchBusy` is the ref that says a batch is in flight; an Escape that arrives
 * while it is held must do nothing at all, rather than half-cancel.
 *
 * Takes the key itself so the "only Escape" half is testable too — the window
 * listener sees every keydown in the app.
 */
export function escapeClosesDialog(state: {
  /** `KeyboardEvent.key`. */
  key: string;
  dialogKind: string;
  /** Whether a batch action's request loop is running (`batchBusy.current`). */
  batchBusy: boolean;
}): boolean {
  return (
    state.key === "Escape" &&
    state.dialogKind !== DIALOG_NONE &&
    !state.batchBusy
  );
}
