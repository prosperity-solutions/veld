/**
 * Which worktrees a client may show, and how it says where the others are.
 *
 * **Extracted for the same reason `acquire.ts` was**: these three answers were
 * inline in `App.tsx`, where a component with a socket and a dozen refs has no
 * test — and two of them were wrong there. The hunt offered worktrees that were
 * on their way off the disk, and the "everything is taken" empty state was
 * *latched*: it was cleared only by the selection changing, so a window that
 * opened into a repo whose one worktree was already on screen elsewhere sat on a
 * CTA through the other client closing and through a worktree being created
 * beside it.
 */

import type { ClientInfo } from "./channel";

/** The least a worktree has to be, to be a candidate. */
export interface OwnableWorktree {
  id: number;
  /** Non-empty while the worktree is in the trash — still on disk, not to be
   *  opened: its panes would root a terminal and a browser at a directory that
   *  is about to stop existing. */
  trashed_at: string;
}

/**
 * The worktrees this client could take.
 *
 * `isRemoving` is passed in rather than read off the row because a window knows
 * about a removal it has just confirmed before the daemon's flag catches up (see
 * `isDeleting` in `App.tsx`), and the two must not disagree about what is
 * openable.
 *
 * `elsewhere` is the claims table; omit it to ask only "is this row openable at
 * all", which is what a hunt wants — the daemon is the authority on who holds
 * what, and a client filtering on its own copy would skip a candidate over a
 * table that is one broadcast stale.
 */
export function openableWorktrees<T extends OwnableWorktree>(
  worktrees: readonly T[],
  isRemoving: (w: T) => boolean,
  elsewhere?: ReadonlyMap<number, unknown>,
): T[] {
  return worktrees.filter(
    (w) => w.trashed_at === "" && !isRemoving(w) && !elsewhere?.has(w.id),
  );
}

/**
 * A stable identity for a set of worktrees, for an effect's dependency list.
 *
 * The point is what it does *not* change on: every 5s poll produces new row
 * objects and a new array, so an effect that depends on the list itself re-runs
 * forever — which is how re-claiming every five seconds got shipped once
 * already. This changes when the *membership* does and not otherwise, which is
 * exactly the condition "look again" should fire on.
 */
export function worktreeSetKey(worktrees: readonly OwnableWorktree[]): string {
  return worktrees.map((w) => w.id).join(",");
}

/**
 * How a rail row says where a worktree is, when it is not here.
 *
 * The holder's own label (`clientLabel`: "Veld Desktop", "Chrome", "another Veld
 * Desktop window"), because that is the phrase the person can act on. The kind
 * is the fallback for a client too old to send a label, and the two are not
 * interchangeable — a desktop window is somewhere a click can *take* you and a
 * browser tab is somewhere you have to go yourself.
 */
export function awayNote(holder: ClientInfo | undefined): string {
  const where =
    holder?.label || (holder?.kind === "electron" ? "another window" : "another client");
  return `open in ${where}`;
}
