/**
 * The prompt a project's New worktree dialog was in the middle of.
 *
 * **A prompt is the most expensive thing in that dialog and it was the only
 * thing with no way back.** Every other field is a word, a picker or a random
 * draw; the prompt is a paragraph somebody composed. Esc, a click on the scrim,
 * the ⋯ close — all three are one gesture away from the field while you are
 * reading what you typed, and all three threw it away. So the field is a draft
 * now: it survives closing the dialog, switching project, and reloading the
 * page, and it goes away when it is *used*.
 *
 * Four properties, in the order they matter:
 *
 * - **Cleared on a create that carried it, not on a create.** Type a prompt,
 *   change your mind, switch to "Start with a name" and make a plain checkout —
 *   the prompt is still an intention you have not spent, so it is still there
 *   next time. Only a create that actually sent it clears it.
 * - **Per project**, like [`lastAgent`], and in this client's own storage for the
 *   same reason: there is no per-repo settings store, and a draft is not a
 *   preference worth a migration. The cost is the same and stated the same way —
 *   it does not follow you to another machine.
 * - **Written on every keystroke**, not on close. A React `onClose` handler
 *   cannot run when the tab is closed or the browser is killed, and those are
 *   exactly the cases a draft is for.
 * - **`""` removes the row.** A field somebody emptied on purpose is not a draft,
 *   and leaving one behind would resurrect text they deleted.
 */

/** Just enough of `Storage` to be faked in a test. */
export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/**
 * The key this project's draft lives under.
 *
 * The repo root goes in verbatim, and unlike `lastWorktreeName` that is
 * injective here: nothing ever appends to this key. That module's roots are
 * suffixed with `.slot.<slot>` by `selectionKeys`, which is what makes *its*
 * keys collidable — a hazard worth writing down there and not worth inheriting
 * here.
 */
export function promptDraftName(repoRoot: string): string {
  return `veld.promptDraft.${repoRoot}`;
}

/**
 * Record what is in the prompt field, or forget it when it is empty.
 *
 * Capped, because this is written from a keystroke handler into a store with a
 * per-origin quota shared with the layouts, the settings mirror and every
 * project's own draft. 8 KB is far more prompt than anyone types and far less
 * than a pasted log; a draft longer than that keeps its first 8 KB rather than
 * failing to save, because the alternative is losing all of it.
 */
export function rememberPromptDraft(
  store: KeyValueStore,
  repoRoot: string,
  text: string,
): void {
  if (repoRoot === "") return;
  try {
    if (text.trim() === "") {
      store.removeItem(promptDraftName(repoRoot));
      return;
    }
    store.setItem(promptDraftName(repoRoot), text.slice(0, MAX_DRAFT_CHARS));
  } catch {
    // Storage unavailable, or the quota is full. The cost is the behaviour this
    // replaces — a closed dialog loses what was in it — so there is nothing
    // useful to report and nothing to fall back to.
  }
}

/** Longest draft kept. See [`rememberPromptDraft`]. */
const MAX_DRAFT_CHARS = 8192;

/** What this project was typing, or `""` if it was not. */
export function recallPromptDraft(store: KeyValueStore, repoRoot: string): string {
  if (repoRoot === "") return "";
  try {
    return store.getItem(promptDraftName(repoRoot)) ?? "";
  } catch {
    return "";
  }
}
