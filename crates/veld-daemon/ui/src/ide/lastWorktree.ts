/**
 * Which worktree a project reopens on.
 *
 * Switching project — the column, `⌘1…⌘9`, ⌘\` , the palette, the top bar's
 * selector — used to clear the worktree selection outright and let the fallback
 * land on the main checkout. That is the right answer exactly once: the first
 * time you open a project. Afterwards the person switching back is going back to
 * whatever they were doing, and being returned to `main` costs a second click
 * every single time.
 *
 * So each project remembers its own last worktree. Three properties are
 * load-bearing:
 *
 * - **The stored value is the worktree's path, not its row id.** Worktree rowids
 *   are reused: delete a checkout and the next one created can be handed the same
 *   number, so a remembered id can silently resolve to a *different* worktree of
 *   the same project. A path is unique in the table and means the same thing
 *   before and after a delete.
 * - **Per window slot, with the unscoped key as the seed** — the same pair
 *   `usePersistedPerWindow` writes for the selection itself, and for the same
 *   reason: pane layouts are per slot, so two windows on one project must be able
 *   to sit on different worktrees. A brand-new slot inherits the last one written
 *   anywhere, which is what makes the *first* switch in a fresh window land
 *   somewhere sensible instead of on `main`.
 * - **Nothing here decides whether the worktree can be shown.** This resolves a
 *   remembered path to a selection key; the claim and the acquire hunt
 *   (`acquire.ts`) still own "is it free, and if not, where do we land". A recall
 *   that pre-filtered on the client's own claims table would skip a worktree over
 *   a table one broadcast stale — and would be a second answer to a question the
 *   daemon already owns.
 */

/** The least a worktree has to be, to be reopened. */
export interface ReopenableWorktree {
  id: number;
  path: string;
  /** Non-empty while the worktree is in the trash. Never reopened: its panes
   *  would root a terminal at a directory that is about to stop existing. */
  trashed_at: string;
}

/** Just enough of `Storage` to be faked in a test. */
export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/**
 * The `usePersistedPerWindow` name this project's memory lives under.
 *
 * The repo root goes in verbatim, and that is not injective once
 * `selectionKeys` appends `.slot.<slot>` to it: a project rooted at a directory
 * literally named `…​.slot.main` would share a key with the `main` slot's entry
 * for the shorter root. Left alone rather than escaped, because the *value* is
 * what makes it harmless — a stored path is only ever resolved against the
 * target project's own worktrees ([`worktreeKeyToReopen`]), so a collided read
 * matches nothing and degrades to "no opinion". A collision cannot open another
 * project's worktree; the fallback chain answers instead.
 */
export function lastWorktreeName(repoRoot: string): string {
  return `veld.lastWorktree.${repoRoot}`;
}

/**
 * Record where this project is, so switching back returns here.
 *
 * Both keys, exactly as `usePersistedPerWindow` writes them. A storage failure is
 * swallowed for the same reason it is there: the cost is landing on the main
 * checkout next time, which is the behaviour this replaces.
 *
 * **Takes the row, not the path** — the one thing here that is enforced by the
 * compiler rather than by the comment above. `String(worktree.id)` is the natural
 * wrong thing to store, it is the same type as the path, and it fails *silently*:
 * `worktreeKeyToReopen` would simply never match, every project would fall back to
 * its main checkout, and nothing in the suite would notice. A parameter that
 * demands the whole row cannot be handed an id.
 */
export function rememberLastWorktree(
  store: KeyValueStore,
  keys: readonly [string, string],
  worktree: ReopenableWorktree,
): void {
  if (worktree.path === "") return;
  try {
    store.setItem(keys[0], worktree.path);
    store.setItem(keys[1], worktree.path);
  } catch {
    // Storage unavailable — see above.
  }
}

/** What this project was last on, `""` if it has never been recorded here. */
export function recallLastWorktree(
  store: KeyValueStore,
  keys: readonly [string, string],
): string {
  try {
    return store.getItem(keys[0]) ?? store.getItem(keys[1]) ?? "";
  } catch {
    return "";
  }
}

/**
 * The selection key to switch to, or `""` for "no opinion — take the fallback".
 *
 * `""` covers every way the memory can be stale: no record at all, a worktree
 * that has since been removed, and one that is in the trash. Each of those must
 * read as *unremembered* rather than as an error, because the caller's fallback
 * (main checkout, then the first row) is a correct answer in all three.
 *
 * Note what that means for the trash, since "no opinion" reads as if the memory
 * merely goes quiet: the fallback landing is granted like any other, so the
 * recorder overwrites the memory with the main checkout on the next render.
 * Trashing your remembered worktree and then reverting it does **not** bring your
 * place back. Deliberate — a memory that survived being un-openable would have to
 * outrank a real, granted landing, and then nothing else could ever move it.
 */
export function worktreeKeyToReopen(
  worktrees: readonly ReopenableWorktree[],
  rememberedPath: string,
): string {
  if (rememberedPath === "") return "";
  const match = worktrees.find(
    (w) => w.path === rememberedPath && !w.trashed_at,
  );
  return match ? String(match.id) : "";
}
