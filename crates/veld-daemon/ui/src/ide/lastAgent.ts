/**
 * Which agent a project's New worktree dialog opens on.
 *
 * The dialog defaults to the first pane the project declares, which is the
 * project's own stated order and a reasonable first answer. It is the wrong
 * answer every time after that: somebody who picked Codex in this repo yesterday
 * is going to pick Codex again today, and re-picking it is a click the dialog
 * does not need to ask for.
 *
 * Three properties, and the first is the one worth arguing about:
 *
 * - **Per project, and stored on the client.** There is no per-repo settings
 *   store to put this in: the `settings` table's `scope` column exists for
 *   exactly this and has been deliberately left unused twice (see
 *   `crates/veld-core/src/db/mod.rs`'s v10 lanes and v12 var-overrides notes),
 *   and the codebase's answer for per-project *user* state is either a column on
 *   the row it describes or client storage. A migration for "which agent I like
 *   in this repo" is not proportionate, so this follows [`lastWorktree`] — the
 *   same shape, the same key prefix, the same swallowed storage failures. The
 *   cost is stated rather than hidden: it does not follow you to another machine.
 * - **The stored value is the pane's `id`, and it is only ever a suggestion.**
 *   A remembered id that the project no longer declares, or whose `requires_bin`
 *   has gone, resolves to nothing and the caller falls back —
 *   `chooseAgent` in `components/dialogs.tsx` owns that, not this module.
 * - **One key, not a per-window pair.** `lastWorktree` is slotted because two
 *   windows on one project must be able to sit on different worktrees; there is
 *   no equivalent reason for two windows to prefer different agents, and a
 *   per-window preference is one that never seems to stick.
 */

/** Just enough of `Storage` to be faked in a test. */
export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/**
 * The key this project's choice lives under.
 *
 * The repo root goes in verbatim, exactly as [`lastWorktreeName`] does it, and
 * is not injective for the same reason — a project rooted at a directory named
 * like another key's suffix could collide. Harmless for the same reason too: the
 * value is resolved against *that* project's own declared panes, so a collided
 * read matches nothing and degrades to "no opinion".
 */
export function lastAgentName(repoRoot: string): string {
  return `veld.lastAgent.${repoRoot}`;
}

/** Remember the agent this project's dialog was used with. */
export function rememberLastAgent(
  store: KeyValueStore,
  repoRoot: string,
  agentId: string,
): void {
  if (repoRoot === "" || agentId === "") return;
  try {
    store.setItem(lastAgentName(repoRoot), agentId);
  } catch {
    // Storage unavailable (a private window, cleared site data). The cost is
    // the dialog opening on the project's first pane, which is where it opened
    // before this existed.
  }
}

/** The agent this project last used, or `""` for "no opinion". */
export function recallLastAgent(store: KeyValueStore, repoRoot: string): string {
  if (repoRoot === "") return "";
  try {
    return store.getItem(lastAgentName(repoRoot)) ?? "";
  } catch {
    return "";
  }
}
