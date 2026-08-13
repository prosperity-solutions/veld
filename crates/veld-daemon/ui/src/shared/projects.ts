/**
 * Project-level answers: which worktrees a project has to say something about,
 * whether another project needs you, and where a project is already open.
 *
 * **Extracted for the reason `ide/ownership.ts` and `ide/acquire.ts` were.** These
 * are decisions over plain values, and inline in `App.tsx` — a component with a
 * socket, a dozen refs and no test — they are decisions nothing can check. The rail
 * already learned this lesson at the worktree layer; this is the same shape one level
 * up, and it arrived with the same class of bug waiting for it (a badge that counts a
 * worktree on its way off the disk, an "open elsewhere" note naming this window).
 *
 * Nothing here talks to the daemon or to the inbox store. The caller passes the rows
 * and the claims table in, and gets back what to render.
 */

import type { RowState } from "../inbox/inbox";

/** The least a worktree has to be for these answers. */
export interface ProjectWorktree {
  id: number;
  repo_root: string;
  /** Non-empty while the worktree is in the trash — see `countable`. */
  trashed_at: string;
}

/** The least a project has to be. */
export interface ProjectRepo {
  root: string;
  worktrees: ProjectWorktree[];
}

/**
 * Whether a worktree's news counts towards its project.
 *
 * **A trashed worktree is silent.** Its directory is on its way off the disk and the
 * rail already refuses to open it, so a project badge lit by one offers a click that
 * cannot land anywhere — the worst kind of notification, because the only way to
 * clear it is to work out that it was never actionable.
 */
function countable(w: ProjectWorktree): boolean {
  return w.trashed_at === "";
}

/** The worktree ids of one project whose news counts. */
export function projectWorktreeIds(repo: ProjectRepo | null): Set<number> {
  const ids = new Set<number>();
  for (const w of repo?.worktrees ?? []) if (countable(w)) ids.add(w.id);
  return ids;
}

/**
 * The worktree ids of every project *except* the one on screen.
 *
 * This is what the closed selector's dot is computed from: the question it answers is
 * "is there news somewhere I am not looking", and the project already selected is —
 * by definition — not somewhere else. Its own news is the rail's job to show.
 *
 * `activeRoot` not matching any project (a stale `?repo=`, a repo removed between
 * polls) yields *every* id rather than none: the user is looking at a fallback
 * selection, so every project genuinely is elsewhere.
 */
export function otherProjectWorktreeIds(
  repos: readonly ProjectRepo[],
  activeRoot: string | null,
): Set<number> {
  const ids = new Set<number>();
  for (const r of repos) {
    if (r.root === activeRoot) continue;
    for (const w of r.worktrees) if (countable(w)) ids.add(w.id);
  }
  return ids;
}

/**
 * Whether a state is worth a dot on the closed selector.
 *
 * **`working` is not.** It is the one state that is not news — nothing happened,
 * something is merely in flight — and the rail already renders it behind an off-by-
 * default setting for that reason. A dot on the top bar is a strictly louder surface
 * than a rail row: it is on screen whatever project you are in, and it is the only
 * thing that will make somebody open a menu. Lighting it for "a build is running in
 * another project" would have it lit approximately always, which costs the dot its
 * whole meaning.
 *
 * Project *rows* inside the menu do render `working` (subject to the same
 * `activity.showWorking` setting the rail uses) — by then the user has opened the
 * menu and is reading a list, where "this one is busy" is information rather than an
 * interruption.
 */
export function isProjectNews(state: RowState | null): boolean {
  return state !== null && state !== "working";
}

/**
 * Where a project is already open, if anywhere: the first of its worktrees that some
 * other client is showing.
 *
 * **First, not a list.** The note it feeds is one line on a menu row ("open in Veld
 * Desktop") or a tooltip on a 44px square, and a project with three worktrees spread
 * over three windows has no single true answer to render there — the useful fact is
 * that *somewhere else has this project*, and the rail's own rows carry the precise
 * answer for anyone who needs it.
 *
 * Ordered by the project's own worktree order rather than by the claims map, so the
 * answer is stable across renders: iterating a `Map` whose insertion order follows
 * whatever order the daemon last broadcast claims in would let the note flip between
 * two equally-true holders on a poll that changed nothing.
 */
export function projectHolder<C>(
  repo: ProjectRepo | null,
  elsewhere: ReadonlyMap<number, C>,
): C | undefined {
  for (const w of repo?.worktrees ?? []) {
    if (!countable(w)) continue;
    const holder = elsewhere.get(w.id);
    if (holder) return holder;
  }
  return undefined;
}

/**
 * How many projects a number key can address.
 *
 * Nine, because ⌘0 is "reset zoom" almost everywhere and a tenth project would
 * have to take it. A tenth project is not unreachable — one click in the column,
 * or ⌘K — it just has no digit.
 */
export const MAX_PROJECT_SHORTCUTS = 9;

/**
 * The project a number key addresses, or `null`.
 *
 * **Position in the displayed list, not an id**, which is the whole reason the
 * order is persisted by the daemon rather than derived per window: ⌘2 has to mean
 * the same project in every window and after every reload, and a list that sorted
 * differently anywhere would make one chord do two things.
 *
 * `digit` is 1-based, as printed on the key.
 */
export function projectForShortcut<R>(repos: readonly R[], digit: number): R | null {
  if (!Number.isInteger(digit) || digit < 1 || digit > MAX_PROJECT_SHORTCUTS) return null;
  return repos[digit - 1] ?? null;
}

/**
 * The other half of the pair, for ⌘` — the project to go back to.
 *
 * `previousRoot` is the last project selected *before* the current one. It is only
 * offered while it is still a project: one removed since is not somewhere to go,
 * and neither is the one already on screen.
 *
 * Falls back to the next project in the list when there is no usable history, so
 * the chord does something the first time it is pressed rather than nothing — and
 * with exactly two projects, the case it exists for, that fallback *is* the toggle.
 */
export function toggleTarget<R extends { root: string }>(
  repos: readonly R[],
  activeRoot: string | null,
  previousRoot: string | null,
): R | null {
  if (repos.length < 2) return null;
  if (previousRoot && previousRoot !== activeRoot) {
    const seen = repos.find((r) => r.root === previousRoot);
    if (seen) return seen;
  }
  const at = repos.findIndex((r) => r.root === activeRoot);
  if (at === -1) return repos[0] ?? null;
  return repos[(at + 1) % repos.length] ?? null;
}

/**
 * A project's initials, for the column's square.
 *
 * Up to two characters taken from word boundaries (`my-api` → `MA`, `veld` → `VE`),
 * because a 44px square fits about that much and two letters separate far more real
 * project names than one does.
 *
 * **Code points, not code units.** `slice(0, 2)` on a name beginning with an emoji
 * or any astral character splits a surrogate pair and renders `�`; spreading the
 * string iterates whole code points instead.
 *
 * A name that is entirely separators has no word to take from and falls back to the
 * raw characters (`---` → `--`). An **empty** name is the one input that yields an
 * empty square — the tooltip still names the project, and a repo with no name is a
 * data anomaly rather than a case to invent a glyph for.
 */
export function projectInitials(name: string): string {
  const trimmed = name.trim();
  const words = trimmed.split(/[\s._\-/\\]+/u).filter(Boolean);
  if (words.length >= 2) return (firstChar(words[0]) + firstChar(words[1])).toUpperCase();
  const one = words[0] ?? trimmed;
  return [...one].slice(0, 2).join("").toUpperCase();
}

function firstChar(word: string): string {
  return [...word].slice(0, 1).join("");
}

/**
 * Move a project to a new index, producing the full order to send.
 *
 * The daemon takes the whole displayed order, so this returns the whole list
 * rather than a pair of indices. Out-of-range indices return the input unchanged —
 * a drop outside the column is not a reorder, and neither is a drag onto itself.
 */
export function reorderedRoots(
  roots: readonly string[],
  from: number,
  to: number,
): string[] {
  const next = [...roots];
  if (from === to) return next;
  if (from < 0 || from >= roots.length) return next;
  if (to < 0 || to >= roots.length) return next;
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}

/**
 * Turn a caret position into a destination index for [`reorderedRoots`].
 *
 * The two are different coordinate systems and conflating them is an off-by-one
 * that only shows up in one direction. A caret sits *between* items, so `insertAt`
 * ranges over `0…length` and means "put it before the item currently at this
 * index". `reorderedRoots` takes the index the moved item should *end up at*, in a
 * list it has already been removed from — so every caret position after the item's
 * own origin shifts down by one.
 *
 * Returns `null` when the move is a no-op: the two carets either side of an item
 * both mean "leave it where it is", and firing a write for that would round-trip
 * the whole order through the daemon to change nothing.
 */
export function dropTargetIndex(from: number, insertAt: number): number | null {
  if (insertAt === from || insertAt === from + 1) return null;
  return insertAt > from ? insertAt - 1 : insertAt;
}
